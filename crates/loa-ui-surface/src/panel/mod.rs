//! The Surfaces tab: the open surfaces as tabs, each drawn by its [`State`], and a launcher that
//! opens a browser, a document, a VM over RFB, or a surface an MCP server declares. The tab
//! opens nothing itself: it emits [`Intent`]s, and the app hands opened surfaces back with
//! [`Panel::attach`].

pub(crate) mod launcher;

use crate::picker::{self, FilePicker};
use crate::tab::{self, Clock, MonotonicClock, State};
use crate::theme::SurfaceKindStyle;
use iced::widget::{button, column, container, row, rule, scrollable};
use iced::{Center, Element, Fill, Task};
use loa_ui_kit::icons::tinted;
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::text::truncate_trailing;
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole, sp};
use loa_ui_kit::widgets::{Tab, tab_strip};
use loa_view::surface::port::{Decoders, DocumentRenderer};
#[cfg(feature = "remote-video")]
use loa_view::surface::VideoStatus;
use loa_view::surface::{AvailableSurface, BrowserStatus, Capture, CaptureStatus, Intent, Surface, SurfaceInfo, SurfaceUri};
use loa_view::theme::Icon;
use std::path::PathBuf;
use std::sync::Arc;

/// Characters of a surface name shown on its tab.
const TAB_LABEL_CHARS: usize = 18;

/// What the tabs of new surfaces get.
#[derive(Clone)]
pub struct Options {
    pub clock: Arc<dyn Clock>,
    /// The decoders of each pixel tab.
    pub decoders: Decoders,
    /// Opens and renders the files of document tabs; its extensions are what the picker lists.
    pub documents: Arc<dyn DocumentRenderer>,
}

impl Options {
    /// Options on the monotonic system clock.
    pub fn new(decoders: Decoders, documents: Arc<dyn DocumentRenderer>) -> Self {
        Self { clock: Arc::new(MonotonicClock::default()), decoders, documents }
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

pub struct Panel {
    options: Options,
    tabs: Vec<OpenTab>,
    /// `None` shows the launcher.
    active: Option<SurfaceUri>,
    available: Vec<AvailableSurface>,
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
        let picker = FilePicker::new(documents_dir, options.documents.extensions());
        Self {
            options,
            tabs: Vec::new(),
            active: None,
            available: Vec::new(),
            browser,
            url: String::new(),
            address: String::new(),
            password: String::new(),
            picker,
            pending: None,
            error: None,
            #[cfg(feature = "remote-video")]
            video: VideoStatus::default(),
            capture: CaptureStatus::default(),
        }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Intent>) {
        let event = match message {
            Message::Surface(uri, message) => {
                let Some(index) = self.index(&uri) else { return (Task::none(), None) };
                let (task, event) = self.tabs[index].state.update(message);
                return (task.map(move |m| Message::Surface(uri.clone(), m)), event);
            }
            Message::Select(uri) if self.index(&uri).is_some() => set(&mut self.active, Some(uri)),
            Message::Select(_) => None,
            Message::Launcher => set(&mut self.active, None),
            Message::Close(uri) => {
                self.detach(&uri);
                Some(Intent::Close(uri))
            }
            Message::Url(url) => set(&mut self.url, url),
            Message::OpenUrl => {
                let url = self.url.trim().to_owned();
                let ready = !url.is_empty() && self.browser != BrowserStatus::Missing;
                ready.then(|| self.start(format!("Opening {url}"), Intent::OpenBrowser(url)))
            }
            Message::Picker(message) => self.picker.update(message).map(|path| self.start(format!("Opening {}", path.display()), Intent::OpenDocument(path))),
            Message::Address(address) => set(&mut self.address, address),
            Message::Password(password) => set(&mut self.password, password),
            Message::Connect => {
                let address = self.address.trim().to_owned();
                (!address.is_empty()).then(|| {
                    let password = Some(std::mem::take(&mut self.password)).filter(|p| !p.is_empty());
                    self.start(format!("Connecting to {address}"), Intent::OpenVm { address, password })
                })
            }
            Message::OpenAvailable(uri) if self.index(&uri).is_some() => set(&mut self.active, Some(uri)),
            Message::OpenAvailable(uri) => Some(self.start(format!("Opening {uri}"), Intent::Open(uri))),
            #[cfg(feature = "remote-video")]
            Message::Video(enabled) => (enabled != self.video.enabled).then(|| {
                self.video.enabled = enabled;
                Intent::SetVideo(enabled)
            }),
            Message::Capture(capture) => {
                let blocked = capture == Capture::VirtualDisplay && self.capture.virtual_display_missing.is_some();
                (capture != self.capture.selected && !blocked).then(|| {
                    self.capture.selected = capture;
                    Intent::SetCapture(capture)
                })
            }
        };
        (Task::none(), event)
    }

    /// Shows `what` as pending and passes `intent` on.
    fn start(&mut self, what: String, intent: Intent) -> Intent {
        self.pending = Some(what);
        self.error = None;
        intent
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

    pub fn set_available(&mut self, available: Vec<AvailableSurface>) {
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
    pub fn available(&self) -> &[AvailableSurface] {
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
fn set<T>(field: &mut T, value: T) -> Option<Intent> {
    *field = value;
    None
}
