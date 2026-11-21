//! The Surfaces tab: the open surfaces as tabs, each drawn by its [`State`], and a launcher that
//! opens a browser, a document, a VM over RFB, or a surface an MCP server declares. The tab
//! opens nothing itself: it emits [`Event`]s, and the app hands opened surfaces back with
//! [`Panel::attach`].

pub(crate) mod launcher;

use crate::decode::FrameDecoder;
use crate::picker::{self, FilePicker};
use crate::tab::{self, Clock, MonotonicClock, State};
use crate::theme::SurfaceKindStyle;
#[cfg(feature = "remote-video")]
pub use crate::video::VideoStatus;
use iced::widget::{button, column, container, row, rule, scrollable};
use iced::{Center, Element, Fill, Task};
use loa_contracts::surface::{Surface, SurfaceInfo, SurfaceUri};
use loa_surfaces::docs::Fonts;
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::text::truncate_trailing;
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole, sp};
use loa_ui_kit::widgets::{Tab, tab_strip};
use std::path::PathBuf;
use std::sync::Arc;

/// Characters of a surface name shown on its tab.
const TAB_LABEL_CHARS: usize = 18;

/// Whether browser surfaces can be opened, as the launcher says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserStatus {
    /// The browser executable, and whether Chrome's sandbox is on.
    Ready { executable: PathBuf, sandboxed: bool },
    Missing,
}

/// Makes the extra decoder (e.g. H.264) of each new pixel tab, tried before the stills.
pub type NewDecoder = Arc<dyn Fn() -> Box<dyn FrameDecoder> + Send + Sync>;

/// How browser frames are captured.
pub use loa_surfaces::CaptureMode as Capture;

/// The browser capture choice, as the launcher shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureStatus {
    pub selected: Capture,
    /// Why the virtual display can't run here; `None` when it can.
    pub virtual_display_missing: Option<String>,
    /// The running browser keeps the other capture until its tabs close.
    pub pending: bool,
}

impl Default for CaptureStatus {
    fn default() -> Self {
        Self { selected: Capture::Screencast, virtual_display_missing: Some("not checked".into()), pending: false }
    }
}

/// A surface that can be opened: a configured VM endpoint, or one an MCP server declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Available {
    pub info: SurfaceInfo,
    /// The server declaring it, e.g. `vm` or the MCP server's name.
    pub server: String,
}

/// What the tabs of new surfaces get.
#[derive(Clone)]
pub struct Options {
    pub clock: Arc<dyn Clock>,
    /// Fonts of rendered PDF and Office pages.
    pub fonts: Fonts,
    /// An extra decoder for pixel tabs; without one they ask video surfaces for stills.
    pub decoder: Option<NewDecoder>,
}

impl Default for Options {
    fn default() -> Self {
        Self { clock: Arc::new(MonotonicClock::default()), fonts: Fonts::none(), decoder: None }
    }
}

struct OpenTab {
    server: String,
    /// The name, cut to fit a tab.
    label: String,
    state: State,
}

#[derive(Clone, Debug)]
pub enum Message {
    /// A message of one open surface's tab.
    Surface(SurfaceUri, tab::Message),
    Select(SurfaceUri),
    /// Show the launcher.
    Launcher,
    Close(SurfaceUri),
    Url(String),
    OpenUrl,
    Picker(picker::Message),
    Address(String),
    Password(String),
    Connect,
    OpenAvailable(SurfaceUri),
    /// Turn live video encoding on or off.
    #[cfg(feature = "remote-video")]
    Video(bool),
    /// Pick how browser frames are captured.
    Capture(Capture),
}

/// What the app must do.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    OpenBrowser(String),
    OpenDocument(PathBuf),
    OpenVm { address: String, password: Option<String> },
    /// Open a listed surface.
    Open(SurfaceUri),
    /// The user closed the tab of an open surface.
    Close(SurfaceUri),
    /// What an open surface's tab asks of the app.
    Surface(tab::Event),
    /// The user turned live video encoding on or off.
    #[cfg(feature = "remote-video")]
    SetVideo(bool),
    /// The user picked how browser frames are captured.
    SetCapture(Capture),
}

pub struct Panel {
    options: Options,
    tabs: Vec<OpenTab>,
    /// `None` shows the launcher.
    active: Option<SurfaceUri>,
    available: Vec<Available>,
    browser: BrowserStatus,
    url: String,
    address: String,
    password: String,
    picker: FilePicker,
    /// What is being opened right now.
    pending: Option<String>,
    error: Option<String>,
    #[cfg(feature = "remote-video")]
    video: VideoStatus,
    capture: CaptureStatus,
}

impl Panel {
    /// `documents_dir` is where the file picker starts.
    pub fn new(options: Options, browser: BrowserStatus, documents_dir: impl Into<PathBuf>) -> Self {
        Self {
            options,
            tabs: Vec::new(),
            active: None,
            available: Vec::new(),
            browser,
            url: String::new(),
            address: String::new(),
            password: String::new(),
            picker: FilePicker::new(documents_dir),
            pending: None,
            error: None,
            #[cfg(feature = "remote-video")]
            video: VideoStatus::default(),
            capture: CaptureStatus::default(),
        }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::Surface(uri, message) => {
                let Some(index) = self.index(&uri) else { return (Task::none(), None) };
                let (task, event) = self.tabs[index].state.update(message);
                return (task.map(move |m| Message::Surface(uri.clone(), m)), event.map(Event::Surface));
            }
            Message::Select(uri) if self.index(&uri).is_some() => set(&mut self.active, Some(uri)),
            Message::Select(_) => None,
            Message::Launcher => set(&mut self.active, None),
            Message::Close(uri) => {
                self.detach(&uri);
                Some(Event::Close(uri))
            }
            Message::Url(url) => set(&mut self.url, url),
            Message::OpenUrl => {
                let url = self.url.trim().to_owned();
                let ready = !url.is_empty() && self.browser != BrowserStatus::Missing;
                ready.then(|| self.start(format!("Opening {url}"), Event::OpenBrowser(url)))
            }
            Message::Picker(message) => self.picker.update(message).map(|path| self.start(format!("Opening {}", path.display()), Event::OpenDocument(path))),
            Message::Address(address) => set(&mut self.address, address),
            Message::Password(password) => set(&mut self.password, password),
            Message::Connect => {
                let address = self.address.trim().to_owned();
                (!address.is_empty()).then(|| {
                    let password = Some(std::mem::take(&mut self.password)).filter(|p| !p.is_empty());
                    self.start(format!("Connecting to {address}"), Event::OpenVm { address, password })
                })
            }
            Message::OpenAvailable(uri) if self.index(&uri).is_some() => set(&mut self.active, Some(uri)),
            Message::OpenAvailable(uri) => Some(self.start(format!("Opening {uri}"), Event::Open(uri))),
            #[cfg(feature = "remote-video")]
            Message::Video(enabled) => (enabled != self.video.enabled).then(|| {
                self.video.enabled = enabled;
                Event::SetVideo(enabled)
            }),
            Message::Capture(capture) => {
                let blocked = capture == Capture::VirtualDisplay && self.capture.virtual_display_missing.is_some();
                (capture != self.capture.selected && !blocked).then(|| {
                    self.capture.selected = capture;
                    Event::SetCapture(capture)
                })
            }
        };
        (Task::none(), event)
    }

    /// Shows `what` as pending and passes `event` on.
    fn start(&mut self, what: String, event: Event) -> Event {
        self.pending = Some(what);
        self.error = None;
        event
    }

    fn index(&self, uri: &SurfaceUri) -> Option<usize> {
        self.tabs.iter().position(|t| &t.state.info().uri == uri)
    }

    /// Shows an opened surface in a new tab and makes it active. The task streams its frames
    /// (or fetches its document) and delivers input.
    pub fn attach(&mut self, surface: Arc<dyn Surface>, server: impl Into<String>) -> Task<Message> {
        let uri = surface.info().uri;
        self.pending = None;
        self.active = Some(uri.clone());
        if self.index(&uri).is_some() {
            return Task::none();
        }
        let (state, task) = State::open(surface, &self.options);
        self.tabs.push(OpenTab { server: server.into(), label: truncate_trailing(&state.info().name, TAB_LABEL_CHARS), state });
        task.map(move |m| Message::Surface(uri.clone(), m))
    }

    /// Drops the tab of `uri`, e.g. when the surface closed elsewhere.
    pub fn detach(&mut self, uri: &SurfaceUri) {
        let Some(index) = self.index(uri) else { return };
        self.tabs.remove(index);
        if self.active.as_ref() == Some(uri) {
            self.active = self.tabs.get(index.saturating_sub(1)).map(|t| t.state.info().uri.clone());
        }
    }

    /// Opening failed: shown on the launcher.
    pub fn failed(&mut self, error: impl Into<String>) {
        self.pending = None;
        self.error = Some(error.into());
        self.active = None;
    }

    pub fn set_available(&mut self, available: Vec<Available>) {
        self.available = available;
    }

    pub fn set_browser(&mut self, browser: BrowserStatus) {
        self.browser = browser;
    }

    /// The active surface, `None` on the launcher.
    pub fn active(&self) -> Option<&SurfaceUri> {
        self.active.as_ref()
    }

    /// The tab of an open surface.
    pub fn surface(&self, uri: &SurfaceUri) -> Option<&State> {
        self.index(uri).map(|i| &self.tabs[i].state)
    }

    #[cfg(feature = "remote-video")]
    pub fn set_video(&mut self, video: VideoStatus) {
        self.video = video;
    }

    #[cfg(feature = "remote-video")]
    pub fn video(&self) -> &VideoStatus {
        &self.video
    }

    pub fn set_capture(&mut self, capture: CaptureStatus) {
        self.capture = capture;
    }

    pub fn capture(&self) -> &CaptureStatus {
        &self.capture
    }

    /// Open surfaces in tab order.
    pub fn open_surfaces(&self) -> impl Iterator<Item = &SurfaceInfo> {
        self.tabs.iter().map(|t| t.state.info())
    }

    pub fn pending(&self) -> Option<&str> {
        self.pending.as_deref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Surfaces the launcher lists for opening.
    pub fn available(&self) -> &[Available] {
        &self.available
    }

    pub fn view(&self) -> Element<'_, Message> {
        let body = match self.active.as_ref().and_then(|uri| self.surface(uri)) {
            Some(state) => {
                let uri = state.info().uri.clone();
                state.view().map(move |m| Message::Surface(uri.clone(), m))
            }
            None => self.launcher(),
        };
        column![self.strip(), rule::horizontal(1).style(style::rule), body].into()
    }

    /// One tab per open surface, then the launcher button.
    fn strip(&self) -> Element<'_, Message> {
        let tabs = self.tabs.iter().map(|t| {
            let info = t.state.info();
            let close = button(tinted(Icon::X, TextRole::Caption, Tone::Muted)).on_press(Message::Close(info.uri.clone()));
            Tab {
                label: &t.label,
                icon: Some(SurfaceKindStyle::of(&t.server, info.mode).icon),
                active: self.active.as_ref() == Some(&info.uri),
                on: Message::Select(info.uri.clone()),
                id: None,
                trailing: Some(close.padding(sp(Sp::Xs)).style(style::button(ButtonKind::Ghost)).into()),
            }
        });
        let thin = scrollable::Scrollbar::new().width(sp(Sp::Xs) / 2.0).scroller_width(sp(Sp::Xs) / 2.0);
        let tabs = scrollable(tab_strip(tabs)).direction(scrollable::Direction::Horizontal(thin)).style(style::scrollbar(true)).width(Fill);
        let launcher = button(tinted(Icon::Plus, TextRole::Body, Tone::Muted)).on_press(Message::Launcher).padding(sp(Sp::Xs));
        let launcher = launcher.style(style::button(ButtonKind::Tab { active: self.active.is_none() }));
        container(row![tabs, launcher].spacing(sp(Sp::Xs)).align_y(Center)).padding([sp(Sp::Xs), sp(Sp::S)]).into()
    }
}

/// Sets a launcher field; nothing for the app to do.
fn set<T>(field: &mut T, value: T) -> Option<Event> {
    *field = value;
    None
}
