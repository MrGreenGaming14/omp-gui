//! The terminal tab: sub tabs, each a terminal on a PTY its [`PtyBackend`] opened.

use crate::{LocalPtyBackend, PtyBackend, RemotePtyBackend, emulator::Emulator};
use crate::{canvas::{CanvasEvent, Metrics, TerminalCanvas}, input::{KeyAction, encode_paste, key_action}, local::LocalShell, palette::TermPalette};
use loa_view::terminal::port::{ChannelRead, ChannelReader, ChannelWriter, GridSize, PtyChannel, RemotePty, Result};
use loa_view::theme::Icon;
use alacritty_terminal::{event::Event as TermEvent, grid::Scroll, term::{ClipboardType, TermMode}};
use bytes::Bytes;
use iced::{task::Handle, widget::{button, canvas, center, column, container, row, rule, text}, Element, Fill, Task, clipboard};
use loa_ui_kit::{icons::tinted, style::{self as kit, ButtonKind, Tone}, theme::{LayoutRole, SpaceRole as Sp, TextRole as T, sp, width}, widgets::label};
use loa_util::LockExt;
use std::{cell::Cell as StdCell, sync::{Arc, Mutex}};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(u64);

/// Where a terminal runs: a local program, or the login shell on a host the SSH layer knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target { Local(LocalShell), Remote { host: String } }
impl Target {
    fn title(&self) -> String {
        match self {
            Target::Local(s) => s.cwd.file_name().map_or_else(|| s.cwd.display().to_string(), |n| n.to_string_lossy().into_owned()),
            Target::Remote { host } => host.clone(),
        }
    }
}

/// `Failed`: opening or driving the PTY failed; the screen stays for reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status { Connecting, Running, Failed(String) }

/// The app sends [`Message::NewTab`], [`Message::Select`] and [`Message::Close`]; the rest come from the tab's widgets and tasks.
#[derive(Debug, Clone)]
pub enum Message {
    NewTab,
    Select(TabId),
    Close(TabId),
    Canvas(TabId, CanvasEvent),
    Paste(TabId, Option<String>),
    /// The opened channel, moved out of the cloneable message exactly once.
    Opened(TabId, Arc<Mutex<Option<PtyChannel>>>),
    /// Program output, or why opening, reading or writing failed; the write side reports `Closed` once its tab is gone.
    Output(TabId, Result<ChannelRead, String>),
}

#[cfg_attr(test, derive(Debug, PartialEq))]
enum Input { Bytes(Bytes), Resize(GridSize) }

/// A sub tab. Dropping `input` ends the write loop, which hangs up; `pending` is the input queued before the channel opened;
/// `_reader` aborts the read loop when the tab is dropped.
struct Tab {
    id: TabId, target: Target, title: String, status: Status, emulator: Emulator, cache: canvas::Cache, drawn_with: StdCell<Option<TermPalette>>,
    input: mpsc::UnboundedSender<Input>, pending: Option<mpsc::UnboundedReceiver<Input>>, _reader: Option<Handle>,
}
impl Tab {
    /// Input is dropped only after the write loop ended, when it has nowhere to go.
    fn send(&self, input: Input) { let _ = self.input.send(input); }
    /// Messages for this tab; a channel that arrives for a closed tab hangs up as it drops.
    fn update(&mut self, message: Message) -> Task<Message> {
        let id = self.id;
        match message {
            Message::Opened(_, channel) => {
                let (Some(PtyChannel { reader, writer }), Some(pending)) = (channel.locked().take(), self.pending.take()) else { return Task::none() };
                self.status = Status::Running;
                let (read, handle) = Task::run(read_stream(reader), move |o| Message::Output(id, o)).abortable();
                self._reader = Some(handle.abort_on_drop());
                return Task::batch([read, Task::perform(write_loop(writer, pending), move |r| Message::Output(id, r.map(|()| ChannelRead::Closed { exit_code: None }).map_err(|e| e.to_string())))]);
            }
            Message::Output(_, Ok(ChannelRead::Data(bytes))) => return self.output(&bytes),
            // The first failure is the one worth reading.
            Message::Output(_, Err(error)) if !matches!(self.status, Status::Failed(_)) => self.status = Status::Failed(error),
            Message::Canvas(_, event) => return self.canvas(event),
            Message::Paste(_, Some(text)) => {
                self.send(Input::Bytes(encode_paste(&text, *self.emulator.term.mode()).into()));
                self.emulator.term.scroll_display(Scroll::Bottom);
                self.cache.clear();
            }
            _ => {}
        }
        Task::none()
    }
    /// Feeds output and acts on what the program asked for: replies, titles, clipboard.
    fn output(&mut self, bytes: &[u8]) -> Task<Message> {
        self.emulator.advance(bytes);
        self.cache.clear();
        Task::batch(self.emulator.take_events().into_iter().filter_map(|event| {
            match event {
                TermEvent::PtyWrite(reply) => self.send(Input::Bytes(reply.into())),
                TermEvent::Title(title) => self.title = title,
                TermEvent::ResetTitle => self.title = self.target.title(),
                TermEvent::ClipboardStore(ClipboardType::Clipboard, t) => return Some(clipboard::write(t)),
                TermEvent::ClipboardStore(ClipboardType::Selection, t) => return Some(clipboard::write_primary(t)),
                TermEvent::TextAreaSizeRequest(format) => self.send(Input::Bytes(format(crate::emulator::window_size(self.emulator.size())).into())),
                _ => {}
            }
            None
        }))
    }
    fn canvas(&mut self, event: CanvasEvent) -> Task<Message> {
        let (id, mode, emulator) = (self.id, *self.emulator.term.mode(), &mut self.emulator);
        match event {
            CanvasEvent::Key { key, modifiers, text } => match key_action(&key, modifiers, text.as_deref(), mode) {
                Some(KeyAction::Send(bytes)) => {
                    let _ = self.input.send(Input::Bytes(bytes));
                    emulator.term.scroll_display(Scroll::Bottom);
                }
                Some(KeyAction::Copy) => return emulator.selection_text().map_or_else(Task::none, clipboard::write),
                Some(KeyAction::Paste) => return clipboard::read().map(move |t| Message::Paste(id, t)),
                Some(KeyAction::Scroll(scroll)) => emulator.term.scroll_display(scroll),
                None => {}
            },
            // Full screen programs (less, vim) get arrow keys for the wheel.
            CanvasEvent::Scroll(lines) if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) && !mode.intersects(TermMode::MOUSE_MODE) => {
                let key = [0x1b, if mode.contains(TermMode::APP_CURSOR) { b'O' } else { b'[' }, if lines > 0 { b'A' } else { b'B' }];
                let _ = self.input.send(Input::Bytes(key.repeat(lines.unsigned_abs() as usize).into()));
            }
            CanvasEvent::Scroll(lines) => emulator.term.scroll_display(Scroll::Delta(lines)),
            CanvasEvent::SelectStart { cell, side, kind } => emulator.select_start(kind, cell, side),
            CanvasEvent::SelectUpdate { cell, side } => emulator.select_update(cell, side),
            CanvasEvent::SelectEnd => return emulator.selection_text().map_or_else(Task::none, clipboard::write_primary),
            CanvasEvent::Resized(_) => {}
        }
        self.cache.clear();
        Task::none()
    }
}

pub struct State { tabs: Vec<Tab>, active: Option<TabId>, next_id: u64, metrics: Metrics, grid: GridSize, new_tab: LocalShell, backend: Arc<dyn PtyBackend> }
impl State {
    /// An empty tab on local PTYs; `new_tab` is what the + button opens.
    pub fn new(metrics: Metrics, new_tab: LocalShell) -> Self {
        Self { tabs: Vec::new(), active: None, next_id: 1, metrics, grid: GridSize::default(), new_tab, backend: Arc::new(LocalPtyBackend) }
    }
    /// Opens remote targets through `remote`, everything else as before.
    pub fn with_remote(self, remote: Arc<dyn RemotePty>) -> Self { Self { backend: Arc::new(RemotePtyBackend { remote, fallback: self.backend.clone() }), ..self } }
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
            Ok(channel) => Message::Opened(id, Arc::new(Mutex::new(Some(channel)))),
            Err(e) => Message::Output(id, Err(e.to_string())),
        }))
    }
    pub fn tabs(&self) -> impl Iterator<Item = TabId> + '_ { self.tabs.iter().map(|t| t.id) }
    pub fn active(&self) -> Option<TabId> { self.active }
    pub fn is_empty(&self) -> bool { self.tabs.is_empty() }
    /// The program's title if it set one, else the directory or host.
    pub fn title(&self, id: TabId) -> Option<&str> { self.tab(id).map(|t| t.title.as_str()) }
    pub fn status(&self, id: TabId) -> Option<&Status> { self.tab(id).map(|t| &t.status) }
    pub fn target(&self, id: TabId) -> Option<&Target> { self.tab(id).map(|t| &t.target) }
    fn tab(&self, id: TabId) -> Option<&Tab> { self.tabs.iter().find(|t| t.id == id) }
    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> { self.tabs.iter_mut().find(|t| t.id == id) }
    /// Shows a sub tab, bringing its grid to the pane's size.
    fn show(&mut self, id: TabId) {
        let grid = self.grid;
        let Some(tab) = self.tab_mut(id) else { return };
        if tab.emulator.resize(grid) {
            tab.send(Input::Resize(grid));
            tab.cache.clear();
        }
        self.active = Some(id);
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::NewTab => return self.open(Target::Local(self.new_tab.clone())).1,
            Message::Select(id) => self.show(id),
            Message::Canvas(id, CanvasEvent::Resized(grid)) => {
                self.grid = grid;
                self.show(id);
            }
            // Dropping the tab hangs up its program; its neighbour becomes active.
            Message::Close(id) | Message::Output(id, Ok(ChannelRead::Closed { .. })) => {
                let Some(index) = self.tabs.iter().position(|t| t.id == id) else { return Task::none() };
                self.tabs.remove(index);
                if self.active == Some(id) {
                    self.active = self.tabs.get(index).or(self.tabs.last()).map(|t| t.id);
                    if let Some(next) = self.active { self.show(next) }
                }
            }
            Message::Canvas(id, _) | Message::Paste(id, _) | Message::Opened(id, _) | Message::Output(id, _) => {
                return self.tab_mut(id).map_or_else(Task::none, |tab| tab.update(message));
            }
        }
        Task::none()
    }
    pub fn view(&self) -> Element<'_, Message> {
        let pad = [sp(Sp::Xs), sp(Sp::S)];
        let ghost = |icon, on| button(tinted(icon, T::Caption, Tone::Muted)).padding(sp(Sp::Xs)).style(kit::button(ButtonKind::Ghost)).on_press(on);
        let tabs = self.tabs.iter().map(|tab| {
            let title = container(label(tab.title.as_str(), T::Label, Tone::Normal).wrapping(text::Wrapping::None)).max_width(width(LayoutRole::LabelColumn)).clip(true);
            let style = kit::button(ButtonKind::Tab { active: self.active == Some(tab.id) });
            button(row![title, ghost(Icon::X, Message::Close(tab.id))].spacing(sp(Sp::Xs)).align_y(iced::Center)).padding(pad).style(style).on_press(Message::Select(tab.id)).into()
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
                column![screen].push(line.map(|(s, tone)| container(label(s, T::Caption, tone)).padding(pad))).into()
            }
        };
        container(column![container(bar).padding(pad).width(Fill), rule::horizontal(1).style(kit::rule), container(body).padding(pad)]).width(Fill).height(Fill).into()
    }
}

impl loa_ui_kit::widgets::PanelPane for State {
    type Message = Message;
    fn id(&self) -> &str { "terminal" }
    fn title(&self) -> &str { "Terminal" }
    fn icon(&self) -> Icon { Icon::Terminal }
    fn view(&self) -> Element<'_, Message> { State::view(self) }
}

/// Reads until the program ends or the channel fails.
fn read_stream(reader: Box<dyn ChannelReader>) -> impl futures::Stream<Item = Result<ChannelRead, String>> + Send {
    futures::stream::unfold(Some(reader), |reader| async move {
        let mut reader = reader?;
        let read = reader.read().await.map_err(|e| e.to_string());
        let more = matches!(read, Ok(ChannelRead::Data(_)));
        Some((read, more.then_some(reader)))
    })
}

/// Forwards input until the tab drops its sender, then hangs up.
async fn write_loop(mut writer: Box<dyn ChannelWriter>, mut input: mpsc::UnboundedReceiver<Input>) -> Result<()> {
    while let Some(item) = input.recv().await {
        match item { Input::Bytes(b) => writer.write(b).await?, Input::Resize(s) => writer.resize(s).await? }
    }
    writer.close().await
}

#[cfg(test)]
mod tests;
