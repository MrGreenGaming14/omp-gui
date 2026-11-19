//! The terminal tab: several terminals as sub tabs, each on a local or
//! remote PTY.

use std::cell::Cell as StdCell;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::term::{ClipboardType, TermMode};
use bytes::Bytes;
use iced::task::Handle;
use iced::widget::{button, canvas, center, column, container, row, text};
use iced::{Element, Fill, Task, clipboard};
use tokio::sync::mpsc;

use crate::canvas::{CanvasEvent, Metrics, TerminalCanvas};
use crate::channel::{ChannelRead, ChannelReader, ChannelWriter, PtyChannel, RemotePty};
use crate::emulator::Emulator;
use crate::input::{KeyAction, encode_paste, key_action};
use crate::local::{self, LocalShell};
use crate::palette::TermPalette;
use crate::{Error, GridSize, Result};

/// Identifies one terminal sub tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(u64);

/// Where a terminal runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A program on this machine.
    Local(LocalShell),
    /// The login shell on a remote host, through the [`RemotePty`] port.
    Remote {
        /// Host name as known to the SSH layer.
        host: String,
    },
}

impl Target {
    fn title(&self) -> String {
        match self {
            Target::Local(shell) => shell
                .cwd
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| shell.cwd.display().to_string()),
            Target::Remote { host } => host.clone(),
        }
    }
}

/// Connection state of a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The PTY is being opened.
    Connecting,
    /// The program is running.
    Running,
    /// Opening or driving the PTY failed; the screen stays for reading.
    Failed(String),
}

/// A value moved through a cloneable message exactly once.
pub struct Handoff<T>(Arc<Mutex<Option<T>>>);

impl<T> Handoff<T> {
    fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }

    fn take(&self) -> Option<T> {
        // A poisoned lock means a panic while holding it; the value is
        // still there and taking it is sound.
        match self.0.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }
}

impl<T> Clone for Handoff<T> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<T> std::fmt::Debug for Handoff<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Handoff")
    }
}

/// Program output as delivered to [`State::update`].
#[derive(Debug, Clone)]
pub enum Output {
    /// A chunk of output.
    Data(Bytes),
    /// The program ended.
    Closed {
        /// Exit status, when known.
        exit_code: Option<u32>,
    },
    /// The channel broke.
    Failed(String),
}

/// Messages of the terminal tab. Only [`Message::NewTab`], [`Message::Select`]
/// and [`Message::Close`] are meant to be sent by the app; the rest come
/// from the tab's own widgets and tasks.
#[derive(Debug, Clone)]
pub enum Message {
    /// Open a local shell with the tab's default settings.
    NewTab,
    /// Show a sub tab.
    Select(TabId),
    /// Close a sub tab and hang up its program.
    Close(TabId),
    /// Input from a terminal canvas.
    Canvas(TabId, CanvasEvent),
    /// Clipboard text to paste.
    Paste(TabId, Option<String>),
    /// The PTY channel is open.
    Opened(TabId, Handoff<PtyChannel>),
    /// The PTY channel could not be opened.
    OpenFailed(TabId, String),
    /// Program output.
    Output(TabId, Output),
    /// The write side stopped, with the error if it failed.
    WriterStopped(TabId, Option<String>),
}

/// What the app must act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The program in a sub tab ended and the sub tab was removed.
    Exited {
        /// The removed sub tab.
        tab: TabId,
        /// Exit status, when known.
        exit_code: Option<u32>,
    },
    /// The user closed a sub tab.
    Closed {
        /// The removed sub tab.
        tab: TabId,
    },
    /// A sub tab's PTY could not be opened or broke.
    Failed {
        /// The affected sub tab.
        tab: TabId,
        /// What went wrong.
        error: String,
    },
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
    /// Input queued before the channel opened; handed to the write loop.
    pending_input: Option<mpsc::UnboundedReceiver<Input>>,
    /// Aborts the read loop when the tab is dropped.
    _reader: Option<Handle>,
}

impl Tab {
    fn send(&self, input: Input) {
        // Fails only after the write loop ended, when input has nowhere to go.
        let _ = self.input.send(input);
    }

    fn redraw(&self) {
        self.cache.clear();
    }
}

/// State of the terminal tab.
pub struct State {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    next_id: u64,
    metrics: Metrics,
    grid: GridSize,
    new_tab: LocalShell,
    remote: Option<Arc<dyn RemotePty>>,
}

impl State {
    /// An empty terminal tab. `new_tab` is what the + button opens.
    pub fn new(metrics: Metrics, new_tab: LocalShell) -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            next_id: 1,
            metrics,
            grid: GridSize::default(),
            new_tab,
            remote: None,
        }
    }

    /// Lets the tab open terminals on remote hosts through `remote`.
    pub fn with_remote(mut self, remote: Arc<dyn RemotePty>) -> Self {
        self.remote = Some(remote);
        self
    }

    /// Changes what the + button opens, for example when the project changes.
    pub fn set_new_tab(&mut self, shell: LocalShell) {
        self.new_tab = shell;
    }

    /// Opens a terminal on `target` in a new sub tab and shows it.
    pub fn open(&mut self, target: Target) -> (TabId, Task<Message>) {
        let id = TabId(self.next_id);
        self.next_id += 1;
        let (input, pending) = mpsc::unbounded_channel();
        self.tabs.push(Tab {
            id,
            title: target.title(),
            target: target.clone(),
            status: Status::Connecting,
            emulator: Emulator::new(self.grid),
            cache: canvas::Cache::new(),
            drawn_with: StdCell::new(None),
            input,
            pending_input: Some(pending),
            _reader: None,
        });
        self.active = Some(id);

        let size = self.grid;
        let finish = move |result: Result<PtyChannel>| match result {
            Ok(channel) => Message::Opened(id, Handoff::new(channel)),
            Err(error) => Message::OpenFailed(id, error.to_string()),
        };
        let task = match target {
            Target::Local(shell) => Task::perform(async move { local::spawn(&shell, size) }, finish),
            Target::Remote { host } => match self.remote.clone() {
                Some(remote) => Task::perform(
                    async move { remote.open_pty(&host, size).await },
                    finish,
                ),
                None => Task::done(finish(Err(Error::NoRemote))),
            },
        };
        (id, task)
    }

    /// Sub tabs in display order.
    pub fn tabs(&self) -> impl Iterator<Item = TabId> + '_ {
        self.tabs.iter().map(|tab| tab.id)
    }

    /// The sub tab on screen.
    pub fn active(&self) -> Option<TabId> {
        self.active
    }

    /// True when no terminal is open.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Title of a sub tab: the program's title if it set one, else the
    /// directory or host name.
    pub fn title(&self, id: TabId) -> Option<&str> {
        self.tab(id).map(|tab| tab.title.as_str())
    }

    /// Connection state of a sub tab.
    pub fn status(&self, id: TabId) -> Option<&Status> {
        self.tab(id).map(|tab| &tab.status)
    }

    /// Where a sub tab runs.
    pub fn target(&self, id: TabId) -> Option<&Target> {
        self.tab(id).map(|tab| &tab.target)
    }

    /// The emulator of a sub tab, for reading its screen.
    pub fn emulator(&self, id: TabId) -> Option<&Emulator> {
        self.tab(id).map(|tab| &tab.emulator)
    }

    fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    /// Removes a sub tab, hanging up its program, and picks the neighbor
    /// as the active one.
    fn remove(&mut self, id: TabId) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        let tab = self.tabs.remove(index);
        tab.send(Input::Close);
        if self.active == Some(id) {
            self.active = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.last())
                .map(|tab| tab.id);
            if let Some(next) = self.active {
                self.fit(next);
            }
        }
        true
    }

    /// Brings a sub tab's grid to the size of the pane.
    fn fit(&mut self, id: TabId) {
        let grid = self.grid;
        if let Some(tab) = self.tab_mut(id)
            && tab.emulator.resize(grid)
        {
            tab.send(Input::Resize(grid));
            tab.redraw();
        }
    }

    fn fail(&mut self, id: TabId, error: String) -> Option<Event> {
        let tab = self.tab_mut(id)?;
        tab.status = Status::Failed(error.clone());
        Some(Event::Failed { tab: id, error })
    }

    /// Handles a message.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::NewTab => {
                let (_, task) = self.open(Target::Local(self.new_tab.clone()));
                (task, None)
            }
            Message::Select(id) => {
                if self.tab(id).is_some() {
                    self.active = Some(id);
                    self.fit(id);
                }
                (Task::none(), None)
            }
            Message::Close(id) => {
                let event = self.remove(id).then_some(Event::Closed { tab: id });
                (Task::none(), event)
            }
            Message::Opened(id, channel) => (self.opened(id, channel), None),
            Message::OpenFailed(id, error) => (Task::none(), self.fail(id, error)),
            Message::Output(id, output) => self.output(id, output),
            Message::WriterStopped(id, error) => {
                let failed = error.and_then(|error| {
                    let running = self.status(id) == Some(&Status::Running);
                    running.then(|| self.fail(id, error)).flatten()
                });
                (Task::none(), failed)
            }
            Message::Canvas(id, event) => (self.canvas(id, event), None),
            Message::Paste(id, text) => {
                if let (Some(tab), Some(text)) = (self.tab_mut(id), text) {
                    let bytes = encode_paste(&text, tab.emulator.mode());
                    tab.send(Input::Bytes(bytes.into()));
                    tab.emulator.scroll(Scroll::Bottom);
                    tab.redraw();
                }
                (Task::none(), None)
            }
        }
    }

    fn opened(&mut self, id: TabId, channel: Handoff<PtyChannel>) -> Task<Message> {
        let Some(PtyChannel { reader, mut writer }) = channel.take() else {
            return Task::none();
        };
        let Some(tab) = self.tab_mut(id) else {
            // Closed while connecting.
            return Task::future(async move {
                let _ = writer.close().await;
            })
            .discard();
        };
        let Some(pending) = tab.pending_input.take() else {
            return Task::none();
        };
        tab.status = Status::Running;
        let (read, handle) =
            Task::run(read_stream(reader), move |output| Message::Output(id, output)).abortable();
        tab._reader = Some(handle.abort_on_drop());
        let write = Task::perform(write_loop(writer, pending), move |result| {
            Message::WriterStopped(id, result.err().map(|error| error.to_string()))
        });
        Task::batch([read, write])
    }

    fn output(&mut self, id: TabId, output: Output) -> (Task<Message>, Option<Event>) {
        match output {
            Output::Data(bytes) => {
                let Some(tab) = self.tab_mut(id) else {
                    return (Task::none(), None);
                };
                tab.emulator.advance(&bytes);
                tab.redraw();
                let events: Vec<TermEvent> = tab.emulator.take_events().collect();
                let mut tasks = Vec::new();
                for event in events {
                    match event {
                        TermEvent::PtyWrite(reply) => tab.send(Input::Bytes(reply.into())),
                        TermEvent::Title(title) => tab.title = title,
                        TermEvent::ResetTitle => tab.title = tab.target.title(),
                        TermEvent::ClipboardStore(ClipboardType::Clipboard, text) => {
                            tasks.push(clipboard::write(text));
                        }
                        TermEvent::ClipboardStore(ClipboardType::Selection, text) => {
                            tasks.push(clipboard::write_primary(text));
                        }
                        TermEvent::TextAreaSizeRequest(format) => {
                            let size = tab.emulator.size();
                            let reply = format(WindowSize {
                                num_lines: size.rows,
                                num_cols: size.cols,
                                cell_width: size.cell_width,
                                cell_height: size.cell_height,
                            });
                            tab.send(Input::Bytes(reply.into()));
                        }
                        _ => {}
                    }
                }
                (Task::batch(tasks), None)
            }
            Output::Closed { exit_code } => {
                let event = self
                    .remove(id)
                    .then_some(Event::Exited { tab: id, exit_code });
                (Task::none(), event)
            }
            Output::Failed(error) => (Task::none(), self.fail(id, error)),
        }
    }

    fn canvas(&mut self, id: TabId, event: CanvasEvent) -> Task<Message> {
        if let CanvasEvent::Resized(grid) = event {
            self.grid = grid;
            self.fit(id);
            return Task::none();
        }
        let Some(tab) = self.tab_mut(id) else {
            return Task::none();
        };
        match event {
            CanvasEvent::Resized(_) => {}
            CanvasEvent::Key { key, modifiers, text } => {
                let mode = tab.emulator.mode();
                match key_action(&key, modifiers, text.as_deref(), mode) {
                    Some(KeyAction::Send(bytes)) => {
                        tab.send(Input::Bytes(Bytes::from(bytes.into_owned())));
                        if tab.emulator.display_offset() != 0 {
                            tab.emulator.scroll(Scroll::Bottom);
                            tab.redraw();
                        }
                    }
                    Some(KeyAction::Copy) => {
                        if let Some(text) = tab.emulator.selection_text() {
                            return clipboard::write(text);
                        }
                    }
                    Some(KeyAction::Paste) => {
                        return clipboard::read().map(move |text| Message::Paste(id, text));
                    }
                    Some(KeyAction::ScrollPageUp) => scroll(tab, Scroll::PageUp),
                    Some(KeyAction::ScrollPageDown) => scroll(tab, Scroll::PageDown),
                    Some(KeyAction::ScrollTop) => scroll(tab, Scroll::Top),
                    Some(KeyAction::ScrollBottom) => scroll(tab, Scroll::Bottom),
                    None => {}
                }
            }
            CanvasEvent::Scroll(lines) => {
                let mode = tab.emulator.mode();
                let alternate = mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
                    && !mode.intersects(TermMode::MOUSE_MODE);
                if alternate {
                    // Full screen programs (less, vim) get arrow keys.
                    let prefix = if mode.contains(TermMode::APP_CURSOR) { b'O' } else { b'[' };
                    let letter = if lines > 0 { b'A' } else { b'B' };
                    let mut bytes = Vec::with_capacity(lines.unsigned_abs() as usize * 3);
                    for _ in 0..lines.unsigned_abs() {
                        bytes.extend_from_slice(&[0x1b, prefix, letter]);
                    }
                    tab.send(Input::Bytes(bytes.into()));
                } else {
                    scroll(tab, Scroll::Delta(lines));
                }
            }
            CanvasEvent::SelectStart { cell, side, kind } => {
                tab.emulator.select_start(kind, cell, side);
                tab.redraw();
            }
            CanvasEvent::SelectUpdate { cell, side } => {
                tab.emulator.select_update(cell, side);
                tab.redraw();
            }
            CanvasEvent::SelectEnd => {
                if let Some(text) = tab.emulator.selection_text() {
                    return clipboard::write_primary(text);
                }
            }
        }
        Task::none()
    }

    /// The sub tab bar and the active terminal.
    pub fn view(&self) -> Element<'_, Message> {
        let mut bar = row![].spacing(4);
        for tab in &self.tabs {
            let style = if self.active == Some(tab.id) {
                button::primary
            } else {
                button::secondary
            };
            let label = row![
                text(tab.title.as_str()),
                button(text("×")).style(button::text).on_press(Message::Close(tab.id)),
            ]
            .spacing(6);
            bar = bar.push(button(label).style(style).on_press(Message::Select(tab.id)));
        }
        bar = bar.push(button(text("+")).style(button::text).on_press(Message::NewTab));

        let body: Element<'_, Message> = match self.active.and_then(|id| self.tab(id)) {
            None => center(text("No terminal open")).into(),
            Some(tab) => {
                let screen = canvas(TerminalCanvas {
                    tab: tab.id,
                    emulator: &tab.emulator,
                    cache: &tab.cache,
                    drawn_with: &tab.drawn_with,
                    metrics: self.metrics,
                })
                .width(Fill)
                .height(Fill);
                match &tab.status {
                    Status::Running => screen.into(),
                    Status::Connecting => column![screen, text(format!("Connecting to {}…", tab.title))]
                        .into(),
                    Status::Failed(error) => column![screen, text(error.as_str()).style(text::danger)]
                        .into(),
                }
            }
        };
        container(column![bar, body].spacing(4)).width(Fill).height(Fill).into()
    }
}

fn scroll(tab: &mut Tab, scroll: Scroll) {
    tab.emulator.scroll(scroll);
    tab.redraw();
}

fn read_stream(reader: Box<dyn ChannelReader>) -> impl futures::Stream<Item = Output> + Send {
    futures::stream::unfold(Some(reader), |reader| async move {
        let mut reader = reader?;
        match reader.read().await {
            Ok(ChannelRead::Data(bytes)) => Some((Output::Data(bytes), Some(reader))),
            Ok(ChannelRead::Closed { exit_code }) => Some((Output::Closed { exit_code }, None)),
            Err(error) => Some((Output::Failed(error.to_string()), None)),
        }
    })
}

async fn write_loop(
    mut writer: Box<dyn ChannelWriter>,
    mut input: mpsc::UnboundedReceiver<Input>,
) -> Result<()> {
    while let Some(item) = input.recv().await {
        match item {
            Input::Bytes(bytes) => writer.write(bytes).await?,
            Input::Resize(size) => writer.resize(size).await?,
            Input::Close => break,
        }
    }
    writer.close().await
}
