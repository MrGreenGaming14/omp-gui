//! The terminal tab: sub tabs, each a terminal on a PTY its [`PtyBackend`] opened.

use crate::canvas::{CanvasEvent, Metrics, TerminalCanvas};
use crate::channel::{ChannelRead, ChannelReader, ChannelWriter, LocalPtyBackend, PtyBackend, PtyChannel, RemotePty, RemotePtyBackend};
use crate::emulator::Emulator;
use crate::input::{KeyAction, encode_paste, key_action};
use crate::local::LocalShell;
use crate::palette::TermPalette;
use crate::{GridSize, Result};
use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::{ClipboardType, TermMode};
use bytes::Bytes;
use iced::task::Handle;
use iced::widget::{button, canvas, center, column, container, row, rule, text};
use iced::{Element, Fill, Task, clipboard};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self as kit, ButtonKind, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole as Sp, TextRole as T, sp, width};
use loa_ui_kit::widgets::{PanelPane, label};
use loa_util::LockExt;
use std::cell::Cell as StdCell;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(u64);

/// Where a terminal runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Local(LocalShell),
    /// The login shell on a host the SSH layer knows.
    Remote { host: String },
}

impl Target {
    fn title(&self) -> String {
        match self {
            Target::Local(s) => s.cwd.file_name().map_or_else(|| s.cwd.display().to_string(), |n| n.to_string_lossy().into_owned()),
            Target::Remote { host } => host.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Running,
    /// Opening or driving the PTY failed; the screen stays for reading.
    Failed(String),
}

/// A value moved through a cloneable message exactly once.
pub struct Handoff<T>(Arc<Mutex<Option<T>>>);

impl<T> Clone for Handoff<T> {
    fn clone(&self) -> Self { Self(Arc::clone(&self.0)) }
}

impl<T> std::fmt::Debug for Handoff<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("Handoff") }
}

#[derive(Debug, Clone)]
pub enum Output {
    Data(Bytes),
    Closed { exit_code: Option<u32> },
    Failed(String),
}

/// The app sends [`Message::NewTab`], [`Message::Select`] and [`Message::Close`]; the rest
/// come from the tab's widgets and tasks.
#[derive(Debug, Clone)]
pub enum Message {
    NewTab,
    Select(TabId),
    Close(TabId),
    Canvas(TabId, CanvasEvent),
    Paste(TabId, Option<String>),
    Opened(TabId, Handoff<PtyChannel>),
    OpenFailed(TabId, String),
    Output(TabId, Output),
    /// The write side stopped, with its error if it failed.
    WriterStopped(TabId, Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The program ended and its sub tab was removed.
    Exited { tab: TabId, exit_code: Option<u32> },
    Closed { tab: TabId },
    Failed { tab: TabId, error: String },
}

enum Input {
    Bytes(Bytes),
    Resize(GridSize),
    Close,
}

struct Tab {
    id: TabId,
    target: Target,
    title: String,
    status: Status,
    emulator: Emulator,
    cache: canvas::Cache,
    drawn_with: StdCell<Option<TermPalette>>,
    input: mpsc::UnboundedSender<Input>,
    /// Input queued before the channel opened, handed to the write loop.
    pending: Option<mpsc::UnboundedReceiver<Input>>,
    /// Aborts the read loop when the tab is dropped.
    _reader: Option<Handle>,
}

impl Tab {
    /// Fails only after the write loop ended, when input has nowhere to go.
    fn send(&self, input: Input) { let _ = self.input.send(input); }

    fn scroll(&mut self, scroll: Scroll) {
        self.emulator.scroll(scroll);
        self.cache.clear();
    }
}

pub struct State {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: u64,
    metrics: Metrics,
    grid: GridSize,
    new_tab: LocalShell,
    backend: Arc<dyn PtyBackend>,
}

impl State {
    /// An empty tab on local PTYs; `new_tab` is what the + button opens.
    pub fn new(metrics: Metrics, new_tab: LocalShell) -> Self {
        Self { tabs: Vec::new(), active: None, next_id: 1, metrics, grid: GridSize::default(), new_tab, backend: Arc::new(LocalPtyBackend) }
    }

    /// Opens remote targets through `remote`, everything else as before.
    pub fn with_remote(self, remote: Arc<dyn RemotePty>) -> Self {
        let fallback = Arc::clone(&self.backend);
        self.with_backend(Arc::new(RemotePtyBackend { remote, fallback }))
    }

    pub fn with_backend(self, backend: Arc<dyn PtyBackend>) -> Self { Self { backend, ..self } }
    pub fn set_new_tab(&mut self, shell: LocalShell) { self.new_tab = shell }
    pub fn new_tab(&self) -> &LocalShell { &self.new_tab }

    /// Opens a terminal on `target` in a new sub tab and shows it.
    pub fn open(&mut self, target: Target) -> (TabId, Task<Message>) {
        let id = TabId(self.next_id);
        self.next_id += 1;
        let (input, pending) = mpsc::unbounded_channel();
        let open = self.backend.open(&target, self.grid);
        let tab = Tab { id, title: target.title(), target, status: Status::Connecting, emulator: Emulator::new(self.grid), cache: canvas::Cache::new(), drawn_with: StdCell::new(None), input, pending: Some(pending), _reader: None };
        self.tabs.push(tab);
        self.active = Some(id);
        (id, Task::perform(open, move |r| match r {
            Ok(channel) => Message::Opened(id, Handoff(Arc::new(Mutex::new(Some(channel))))),
            Err(e) => Message::OpenFailed(id, e.to_string()),
        }))
    }

    pub fn tabs(&self) -> impl Iterator<Item = TabId> + '_ { self.tabs.iter().map(|t| t.id) }
    pub fn active(&self) -> Option<TabId> { self.active }
    pub fn is_empty(&self) -> bool { self.tabs.is_empty() }
    /// The program's title if it set one, else the directory or host.
    pub fn title(&self, id: TabId) -> Option<&str> { self.tab(id).map(|t| t.title.as_str()) }
    pub fn status(&self, id: TabId) -> Option<&Status> { self.tab(id).map(|t| &t.status) }
    pub fn target(&self, id: TabId) -> Option<&Target> { self.tab(id).map(|t| &t.target) }
    pub fn emulator(&self, id: TabId) -> Option<&Emulator> { self.tab(id).map(|t| &t.emulator) }
    fn tab(&self, id: TabId) -> Option<&Tab> { self.tabs.iter().find(|t| t.id == id) }
    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> { self.tabs.iter_mut().find(|t| t.id == id) }

    /// Removes a sub tab, hanging up its program; its neighbour becomes active.
    fn remove(&mut self, id: TabId) -> bool {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else { return false };
        self.tabs.remove(index).send(Input::Close);
        if self.active == Some(id) {
            self.active = self.tabs.get(index).or(self.tabs.last()).map(|t| t.id);
            if let Some(next) = self.active { self.fit(next) }
        }
        true
    }

    /// Brings a sub tab's grid to the pane's size.
    fn fit(&mut self, id: TabId) {
        let grid = self.grid;
        if let Some(tab) = self.tab_mut(id).filter(|t| t.emulator.size() != grid) {
            tab.emulator.resize(grid);
            tab.send(Input::Resize(grid));
            tab.cache.clear();
        }
    }

    fn fail(&mut self, id: TabId, error: String) -> Option<Event> {
        self.tab_mut(id)?.status = Status::Failed(error.clone());
        Some(Event::Failed { tab: id, error })
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::NewTab => return (self.open(Target::Local(self.new_tab.clone())).1, None),
            Message::Select(id) => {
                if self.tab(id).is_some() {
                    self.active = Some(id);
                    self.fit(id);
                }
                None
            }
            Message::Close(id) => self.remove(id).then_some(Event::Closed { tab: id }),
            Message::Opened(id, channel) => return (self.opened(id, channel), None),
            Message::OpenFailed(id, error) => self.fail(id, error),
            Message::Output(id, Output::Data(bytes)) => return (self.output(id, &bytes), None),
            Message::Output(id, Output::Closed { exit_code }) => self.remove(id).then_some(Event::Exited { tab: id, exit_code }),
            Message::Output(id, Output::Failed(error)) => self.fail(id, error),
            Message::WriterStopped(id, error) => error.filter(|_| self.status(id) == Some(&Status::Running)).and_then(|e| self.fail(id, e)),
            Message::Canvas(id, event) => return (self.canvas(id, event), None),
            Message::Paste(id, text) => {
                if let (Some(tab), Some(text)) = (self.tab_mut(id), text) {
                    tab.send(Input::Bytes(encode_paste(&text, tab.emulator.mode()).into()));
                    tab.scroll(Scroll::Bottom);
                }
                None
            }
        };
        (Task::none(), event)
    }

    fn opened(&mut self, id: TabId, channel: Handoff<PtyChannel>) -> Task<Message> {
        let Some(PtyChannel { reader, mut writer }) = channel.0.locked().take() else { return Task::none() };
        // Closed while connecting.
        let Some(tab) = self.tab_mut(id) else { return Task::future(async move { let _ = writer.close().await; }).discard() };
        let Some(pending) = tab.pending.take() else { return Task::none() };
        tab.status = Status::Running;
        let (read, handle) = Task::run(read_stream(reader), move |o| Message::Output(id, o)).abortable();
        tab._reader = Some(handle.abort_on_drop());
        Task::batch([read, Task::perform(write_loop(writer, pending), move |r| Message::WriterStopped(id, r.err().map(|e| e.to_string())))])
    }

    /// Feeds output and acts on what the program asked for: replies, titles, clipboard.
    fn output(&mut self, id: TabId, bytes: &[u8]) -> Task<Message> {
        let Some(tab) = self.tab_mut(id) else { return Task::none() };
        tab.emulator.advance(bytes);
        tab.cache.clear();
        let events: Vec<TermEvent> = tab.emulator.take_events().collect();
        let mut tasks = Vec::new();
        for event in events {
            match event {
                TermEvent::PtyWrite(reply) => tab.send(Input::Bytes(reply.into())),
                TermEvent::Title(title) => tab.title = title,
                TermEvent::ResetTitle => tab.title = tab.target.title(),
                TermEvent::ClipboardStore(ClipboardType::Clipboard, t) => tasks.push(clipboard::write(t)),
                TermEvent::ClipboardStore(ClipboardType::Selection, t) => tasks.push(clipboard::write_primary(t)),
                TermEvent::TextAreaSizeRequest(format) => {
                    let s = tab.emulator.size();
                    tab.send(Input::Bytes(format(WindowSize { num_lines: s.rows, num_cols: s.cols, cell_width: s.cell_width, cell_height: s.cell_height }).into()));
                }
                _ => {}
            }
        }
        Task::batch(tasks)
    }

    fn canvas(&mut self, id: TabId, event: CanvasEvent) -> Task<Message> {
        if let CanvasEvent::Resized(grid) = event {
            self.grid = grid;
            self.fit(id);
            return Task::none();
        }
        let Some(tab) = self.tab_mut(id) else { return Task::none() };
        let mode = tab.emulator.mode();
        match event {
            CanvasEvent::Key { key, modifiers, text } => match key_action(&key, modifiers, text.as_deref(), mode) {
                Some(KeyAction::Send(bytes)) => {
                    tab.send(Input::Bytes(Bytes::from(bytes.into_owned())));
                    if tab.emulator.display_offset() != 0 { tab.scroll(Scroll::Bottom) }
                }
                Some(KeyAction::Copy) => return tab.emulator.selection_text().map_or_else(Task::none, clipboard::write),
                Some(KeyAction::Paste) => return clipboard::read().map(move |t| Message::Paste(id, t)),
                Some(KeyAction::ScrollPageUp) => tab.scroll(Scroll::PageUp),
                Some(KeyAction::ScrollPageDown) => tab.scroll(Scroll::PageDown),
                Some(KeyAction::ScrollTop) => tab.scroll(Scroll::Top),
                Some(KeyAction::ScrollBottom) => tab.scroll(Scroll::Bottom),
                None => {}
            },
            // Full screen programs (less, vim) get arrow keys for the wheel.
            CanvasEvent::Scroll(lines) if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) && !mode.intersects(TermMode::MOUSE_MODE) => {
                let key = [0x1b, if mode.contains(TermMode::APP_CURSOR) { b'O' } else { b'[' }, if lines > 0 { b'A' } else { b'B' }];
                tab.send(Input::Bytes(key.repeat(lines.unsigned_abs() as usize).into()));
            }
            CanvasEvent::Scroll(lines) => tab.scroll(Scroll::Delta(lines)),
            CanvasEvent::SelectStart { cell, side, kind } => {
                tab.emulator.select_start(kind, cell, side);
                tab.cache.clear();
            }
            CanvasEvent::SelectUpdate { cell, side } => {
                tab.emulator.select_update(cell, side);
                tab.cache.clear();
            }
            CanvasEvent::SelectEnd => return tab.emulator.selection_text().map_or_else(Task::none, clipboard::write_primary),
            CanvasEvent::Resized(_) => {}
        }
        Task::none()
    }

    /// The sub tab bar and the active terminal.
    pub fn view(&self) -> Element<'_, Message> {
        let ghost = |icon, on| button(tinted(icon, T::Caption, Tone::Muted)).padding(sp(Sp::Xs)).style(kit::button(ButtonKind::Ghost)).on_press(on);
        let tabs = self.tabs.iter().map(|tab| {
            let title = container(label(tab.title.as_str(), T::Label, Tone::Normal).wrapping(text::Wrapping::None)).max_width(width(LayoutRole::LabelColumn)).clip(true);
            let style = kit::button(ButtonKind::Tab { active: self.active == Some(tab.id) });
            button(row![title, ghost(Icon::X, Message::Close(tab.id))].spacing(sp(Sp::Xs)).align_y(iced::Center)).padding([sp(Sp::Xs), sp(Sp::S)]).style(style).on_press(Message::Select(tab.id)).into()
        });
        let bar = row(tabs).push(ghost(Icon::Plus, Message::NewTab)).spacing(sp(Sp::Xs)).align_y(iced::Center);
        let body: Element<'_, Message> = match self.active.and_then(|id| self.tab(id)) {
            None => {
                let open = button(label(format!("Open a terminal in {}", Target::Local(self.new_tab.clone()).title()), T::Label, Tone::Normal)).padding([sp(Sp::Xs), sp(Sp::M)]).style(kit::button(ButtonKind::Secondary));
                center(column![label("No terminal open", T::Label, Tone::Muted), open.on_press(Message::NewTab)].spacing(sp(Sp::S)).align_x(iced::Center)).into()
            }
            Some(tab) => {
                let screen = canvas(TerminalCanvas { tab: tab.id, emulator: &tab.emulator, cache: &tab.cache, drawn_with: &tab.drawn_with, metrics: self.metrics }).width(Fill).height(Fill);
                let line = match &tab.status {
                    Status::Running => None,
                    Status::Connecting => Some((format!("Connecting to {}…", tab.title), Tone::Muted)),
                    Status::Failed(error) => Some((error.clone(), Tone::Danger)),
                };
                column![screen].push(line.map(|(s, tone)| container(label(s, T::Caption, tone)).padding([sp(Sp::Xs), sp(Sp::S)]))).into()
            }
        };
        let bar = container(bar).padding([sp(Sp::Xs), sp(Sp::S)]).width(Fill);
        container(column![bar, rule::horizontal(1).style(kit::rule), container(body).padding([sp(Sp::Xs), sp(Sp::S)])]).width(Fill).height(Fill).into()
    }
}

impl PanelPane for State {
    type Message = Message;
    fn id(&self) -> &str { "terminal" }
    fn title(&self) -> &str { "Terminal" }
    fn icon(&self) -> Icon { Icon::Terminal }
    fn view(&self) -> Element<'_, Message> { State::view(self) }
}

fn read_stream(reader: Box<dyn ChannelReader>) -> impl futures::Stream<Item = Output> + Send {
    futures::stream::unfold(Some(reader), |reader| async move {
        let mut reader = reader?;
        Some(match reader.read().await {
            Ok(ChannelRead::Data(bytes)) => (Output::Data(bytes), Some(reader)),
            Ok(ChannelRead::Closed { exit_code }) => (Output::Closed { exit_code }, None),
            Err(e) => (Output::Failed(e.to_string()), None),
        })
    })
}

async fn write_loop(mut writer: Box<dyn ChannelWriter>, mut input: mpsc::UnboundedReceiver<Input>) -> Result<()> {
    while let Some(item) = input.recv().await {
        match item {
            Input::Bytes(b) => writer.write(b).await?,
            Input::Resize(s) => writer.resize(s).await?,
            Input::Close => break,
        }
    }
    writer.close().await
}

#[cfg(test)]
mod tests;
