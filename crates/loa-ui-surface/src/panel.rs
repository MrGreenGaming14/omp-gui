//! The Surfaces tab: the open surfaces as tabs, each drawn by the generic
//! renderer ([`State`]), and a launcher that opens a browser at a URL, a
//! document from a file, a VM or desktop over RFB, or a surface an MCP
//! server declares.
//!
//! The tab doesn't open anything itself: it emits [`Event`]s, the app opens
//! the surface with its surface service and hands it back with
//! [`Panel::attach`].

use crate::decode::VideoDecoder;
use crate::decode::encoding_name;
use crate::picker::{self, FilePicker};
use crate::state::{self, Clock, MonotonicClock, Options, State};
use crate::style;
use crate::document::PageRenderers;
use iced::widget::{Column, button, column, container, row, rule, scrollable, space, text, text_input};
use iced::{Center, Element, Fill, Task};
use loa_contracts::surface::{Encoding, StreamInfo, Surface, SurfaceInfo, SurfaceMode, SurfaceUri};
use loa_ui_icons::{Icon, muted};
use loa_ui_shell::theme::style as shell;
use std::path::PathBuf;
use std::sync::Arc;

/// Whether browser surfaces can be opened, as the launcher says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserStatus {
    /// The browser executable, and whether Chrome's sandbox is on.
    Ready { executable: PathBuf, sandboxed: bool },
    Missing,
}

/// Makes the video decoder of each new pixel tab.
pub type NewDecoder = Arc<dyn Fn() -> Box<dyn VideoDecoder> + Send + Sync>;

/// Live video encoding for viewers across the network, as the launcher
/// shows it. Tabs in this app always get the source frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoStatus {
    /// Live surfaces stream video to network viewers that decode it.
    pub enabled: bool,
    /// What network viewers get; `None` while the machine's encoders are
    /// tried.
    pub encoder: Option<StreamInfo>,
    /// Hardware encoders that work here, e.g. `h264_vaapi`.
    pub hardware: Vec<String>,
}

/// How browser frames are captured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Capture {
    /// Headless Chrome's own JPEG screencast.
    #[default]
    Screencast,
    /// Headed Chrome on an Xvfb virtual display, captured raw.
    VirtualDisplay,
}

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

/// A surface that can be opened: a configured VM endpoint, or one an MCP
/// server declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Available {
    pub info: SurfaceInfo,
    /// The server declaring it, e.g. `vm` or the MCP server's name.
    pub server: String,
}

struct OpenTab {
    server: String,
    state: State,
}

#[derive(Clone, Debug)]
pub enum Message {
    /// A message of one open surface's renderer.
    Surface(SurfaceUri, state::Message),
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
    /// A surface streams an encoding the renderer can't show; ask its server
    /// for one of `supported`.
    EncodingUnsupported { uri: SurfaceUri, encoding: Encoding, supported: Vec<Encoding> },
    /// The user turned live video encoding on or off.
    SetVideo(bool),
    /// The user picked how browser frames are captured.
    SetCapture(Capture),
}

/// What renderers of new tabs get.
#[derive(Clone)]
pub struct PanelOptions {
    pub clock: Arc<dyn Clock>,
    /// Renders PDF and Office documents.
    pub documents: Option<Arc<dyn PageRenderers>>,
    /// Decodes video in pixel tabs; without it they ask for stills.
    pub video: Option<NewDecoder>,
}

impl Default for PanelOptions {
    fn default() -> Self {
        Self { clock: Arc::new(MonotonicClock::default()), documents: None, video: None }
    }
}

pub struct Panel {
    options: PanelOptions,
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
    video: VideoStatus,
    capture: CaptureStatus,
}

impl Panel {
    /// `documents_dir` is where the file picker starts.
    pub fn new(options: PanelOptions, browser: BrowserStatus, documents_dir: impl Into<PathBuf>) -> Self {
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
            video: VideoStatus::default(),
            capture: CaptureStatus::default(),
        }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Surface(uri, message) => {
                let Some(tab) = self.tab_mut(&uri) else { return (Task::none(), None) };
                let (task, event) = tab.state.update(message);
                let event = event.map(|state::Event::EncodingUnsupported { uri, encoding, supported }| {
                    Event::EncodingUnsupported { uri, encoding, supported }
                });
                return (task.map(move |m| Message::Surface(uri.clone(), m)), event);
            }
            Message::Select(uri) => {
                if self.tab_mut(&uri).is_some() {
                    self.active = Some(uri);
                }
            }
            Message::Launcher => self.active = None,
            Message::Close(uri) => {
                self.detach(&uri);
                return (Task::none(), Some(Event::Close(uri)));
            }
            Message::Url(url) => self.url = url,
            Message::OpenUrl => {
                let url = self.url.trim().to_owned();
                if url.is_empty() || self.browser == BrowserStatus::Missing {
                    return (Task::none(), None);
                }
                self.start(format!("Opening {url}"));
                return (Task::none(), Some(Event::OpenBrowser(url)));
            }
            Message::Picker(message) => {
                if let Some(path) = self.picker.update(message) {
                    self.start(format!("Opening {}", path.display()));
                    return (Task::none(), Some(Event::OpenDocument(path)));
                }
            }
            Message::Address(address) => self.address = address,
            Message::Password(password) => self.password = password,
            Message::Connect => {
                let address = self.address.trim().to_owned();
                if address.is_empty() {
                    return (Task::none(), None);
                }
                let password = Some(std::mem::take(&mut self.password)).filter(|p| !p.is_empty());
                self.start(format!("Connecting to {address}"));
                return (Task::none(), Some(Event::OpenVm { address, password }));
            }
            Message::OpenAvailable(uri) => {
                if self.tab_mut(&uri).is_some() {
                    self.active = Some(uri);
                    return (Task::none(), None);
                }
                self.start(format!("Opening {uri}"));
                return (Task::none(), Some(Event::Open(uri)));
            }
            Message::Video(enabled) => {
                if enabled != self.video.enabled {
                    self.video.enabled = enabled;
                    return (Task::none(), Some(Event::SetVideo(enabled)));
                }
            }
            Message::Capture(capture) => {
                let blocked = capture == Capture::VirtualDisplay && self.capture.virtual_display_missing.is_some();
                if capture != self.capture.selected && !blocked {
                    self.capture.selected = capture;
                    return (Task::none(), Some(Event::SetCapture(capture)));
                }
            }
        }
        (Task::none(), None)
    }

    fn start(&mut self, what: String) {
        self.pending = Some(what);
        self.error = None;
    }

    fn tab_mut(&mut self, uri: &SurfaceUri) -> Option<&mut OpenTab> {
        self.tabs.iter_mut().find(|t| &t.state.info().uri == uri)
    }

    /// Shows an opened surface in a new tab and makes it active. The task
    /// streams its frames (or fetches its document) and delivers input.
    pub fn attach(&mut self, surface: Arc<dyn Surface>, server: impl Into<String>) -> Task<Message> {
        let uri = surface.info().uri;
        self.pending = None;
        self.active = Some(uri.clone());
        if self.tab_mut(&uri).is_some() {
            return Task::none();
        }
        let video = self.options.video.as_ref().map(|make| make());
        let options = Options { clock: self.options.clock.clone(), video, documents: self.options.documents.clone() };
        let (state, task) = State::open(surface, options);
        self.tabs.push(OpenTab { server: server.into(), state });
        task.map(move |m| Message::Surface(uri.clone(), m))
    }

    /// Drops the tab of `uri`, e.g. when the surface closed elsewhere.
    pub fn detach(&mut self, uri: &SurfaceUri) {
        let Some(index) = self.tabs.iter().position(|t| &t.state.info().uri == uri) else { return };
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

    /// The renderer of an open surface.
    pub fn surface(&self, uri: &SurfaceUri) -> Option<&State> {
        self.tabs.iter().find(|t| &t.state.info().uri == uri).map(|t| &t.state)
    }

    pub fn set_video(&mut self, video: VideoStatus) {
        self.video = video;
    }

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
        let active = self.active.as_ref().and_then(|uri| self.tabs.iter().find(|t| &t.state.info().uri == uri));
        let body: Element<'_, Message> = match active {
            Some(tab) => {
                let uri = tab.state.info().uri.clone();
                tab.state.view().map(move |m| Message::Surface(uri.clone(), m))
            }
            None => self.launcher(),
        };
        column![self.strip(), rule::horizontal(1).style(shell::divider), body].into()
    }

    /// One chip per open surface, then the launcher button.
    fn strip(&self) -> Element<'_, Message> {
        let mut chips = row![].spacing(4).align_y(Center);
        for tab in &self.tabs {
            let info = tab.state.info();
            let selected = self.active.as_ref() == Some(&info.uri);
            let icon = match (info.mode, tab.server.as_str()) {
                (SurfaceMode::Document, _) => Icon::FileText,
                (_, "vm") => Icon::Monitor,
                (_, "browser") => Icon::Globe,
                _ => Icon::Square,
            };
            let title = tab.state.tab().title;
            let label = row![muted(icon, 13.0), text(ellipsize(&title, 18)).size(style::TEXT)].spacing(6).align_y(Center);
            let select = button(label)
                .on_press(Message::Select(info.uri.clone()))
                .padding([4, 8])
                .style(shell::row_for(selected));
            let close = button(muted(Icon::X, 12.0)).on_press(Message::Close(info.uri.clone())).padding(4).style(shell::row);
            chips = chips.push(row![select, close].align_y(Center));
        }
        let launcher = button(muted(Icon::Plus, 14.0))
            .on_press(Message::Launcher)
            .padding(6)
            .style(shell::row_for(self.active.is_none()));
        container(
            row![scrollable(chips).direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(2).scroller_width(2))).width(Fill), launcher]
                .spacing(4)
                .align_y(Center),
        )
        .padding([6, 8])
        .into()
    }

    fn launcher(&self) -> Element<'_, Message> {
        let mut content = Column::new().spacing(18).padding([12, 14]);
        if let Some(pending) = &self.pending {
            content = content.push(text(format!("{pending}…")).size(style::TEXT).style(shell::muted));
        }
        if let Some(error) = &self.error {
            content = content.push(
                container(row![loa_ui_icons::icon(Icon::TriangleAlert, 14.0), text(error).size(style::TEXT).style(style::danger)].spacing(8))
                    .padding([8, 10])
                    .width(Fill)
                    .style(shell::card),
            );
        }

        let browser_ready = matches!(self.browser, BrowserStatus::Ready { .. });
        let url = text_input("Search or enter a URL", &self.url)
            .on_input(Message::Url)
            .size(style::TEXT)
            .padding([6, 8])
            .style(shell::input);
        let url = if browser_ready { url.on_submit(Message::OpenUrl) } else { url };
        let open = button(text("Open").size(style::TEXT))
            .on_press_maybe((browser_ready && !self.url.trim().is_empty()).then_some(Message::OpenUrl))
            .padding([6, 12])
            .style(style::action);
        let headed = self.capture.selected == Capture::VirtualDisplay;
        let browser_note: Element<'_, Message> = match &self.browser {
            BrowserStatus::Ready { executable, sandboxed } => {
                let sandbox = if *sandboxed { "" } else { ", without Chrome's sandbox (this kernel restricts user namespaces)" };
                let kind = if headed { "Headed" } else { "Headless" };
                text(format!("{kind} {}{sandbox}", browser_name(executable))).size(style::SMALL).style(shell::muted).into()
            }
            BrowserStatus::Missing => text("No Chrome or Chromium installed. Install one, or run `npx playwright install chromium`.")
                .size(style::SMALL)
                .style(style::warning)
                .into(),
        };
        content = content.push(section(
            Icon::Globe,
            "Browser",
            column![row![url, open].spacing(6).align_y(Center), browser_note, self.capture_choice()].spacing(6),
        ));
        content = content.push(section(Icon::Monitor, "Live video", self.video_status()));

        content = content.push(section(
            Icon::FileText,
            "Document",
            column![
                self.picker.view().map(Message::Picker),
                text("PDF, XLSX, DOCX, PPTX, PNG, JPEG, GIF, WebP and BMP. Pages render here, so scrolling and zoom stay local.")
                    .size(style::SMALL)
                    .style(shell::muted),
            ]
            .spacing(6),
        ));

        let address = text_input("host:port, e.g. localhost:5900", &self.address)
            .on_input(Message::Address)
            .on_submit(Message::Connect)
            .size(style::TEXT)
            .padding([6, 8])
            .style(shell::input);
        let password = text_input("Password (if the server asks)", &self.password)
            .on_input(Message::Password)
            .on_submit(Message::Connect)
            .secure(true)
            .size(style::TEXT)
            .padding([6, 8])
            .style(shell::input);
        let connect = button(text("Connect").size(style::TEXT))
            .on_press_maybe((!self.address.trim().is_empty()).then_some(Message::Connect))
            .padding([6, 12])
            .style(style::action);
        let (vms, declared): (Vec<&Available>, Vec<&Available>) =
            self.available.iter().partition(|a| a.server == VM_SERVER);
        let mut vm = column![address, row![password, connect].spacing(6).align_y(Center)].spacing(6);
        if !vms.is_empty() {
            vm = vm.push(available_rows(&vms));
        }
        content = content.push(section(Icon::Monitor, "VM or desktop (VNC)", vm));

        let declared: Element<'_, Message> = if declared.is_empty() {
            text("No MCP server declares a surface. Surface servers added in Tools show up here.")
                .size(style::SMALL)
                .style(shell::muted)
                .into()
        } else {
            available_rows(&declared)
        };
        content = content.push(section(Icon::Wrench, "From MCP servers", declared));
        scrollable(content).height(Fill).into()
    }

    /// The two browser captures; the virtual display only where it runs.
    fn capture_choice(&self) -> Element<'_, Message> {
        let choice = |label: &'static str, capture: Capture, enabled: bool| {
            button(text(label).size(style::SMALL))
                .on_press_maybe(enabled.then_some(Message::Capture(capture)))
                .padding([3, 8])
                .style(shell::row_for(self.capture.selected == capture))
        };
        let missing = self.capture.virtual_display_missing.as_deref();
        let choices = row![
            text("Capture").size(style::SMALL).style(shell::muted),
            choice("Headless screencast", Capture::Screencast, true),
            choice("Headed on a virtual display", Capture::VirtualDisplay, missing.is_none()),
        ]
        .spacing(6)
        .align_y(Center);
        let note = match (missing, self.capture.selected, self.capture.pending) {
            (Some(why), _, _) => Some(text(format!("Virtual display unavailable: {why}")).size(style::SMALL).style(style::warning)),
            (None, _, true) => Some(
                text("The open browser tabs keep the current capture; new tabs use this one once they close.")
                    .size(style::SMALL)
                    .style(shell::muted),
            ),
            (None, Capture::VirtualDisplay, false) => Some(
                text("Each tab runs headed Chrome on its own Xvfb display, captured raw.").size(style::SMALL).style(shell::muted),
            ),
            (None, Capture::Screencast, false) => None,
        };
        let mut choice = column![choices].spacing(4);
        if let Some(note) = note {
            choice = choice.push(note);
        }
        choice.into()
    }

    /// Whether live surfaces stream video to network viewers, and with
    /// which encoder.
    fn video_status(&self) -> Element<'_, Message> {
        let toggle = |label: &'static str, on: bool| {
            button(text(label).size(style::SMALL))
                .on_press(Message::Video(on))
                .padding([3, 8])
                .style(shell::row_for(self.video.enabled == on))
        };
        let choices = row![
            text("Encode browser and VM frames as video for peers").size(style::SMALL).style(shell::muted),
            space::horizontal(),
            toggle("On", true),
            toggle("Off", false),
        ]
        .spacing(6)
        .align_y(Center);
        let encoder = match &self.video.encoder {
            _ if !self.video.enabled => "Off: peers get frames as their source makes them (raw pixels, JPEG screencast)".to_owned(),
            None => "Trying this machine's encoders…".to_owned(),
            Some(stream) if stream.hardware => {
                format!("{} with {}, hardware encoded", encoding_name(stream.encoding), stream.encoder)
            }
            Some(stream) => format!(
                "No hardware encoder works here: {} with {}, software encoded",
                encoding_name(stream.encoding),
                stream.encoder
            ),
        };
        let mut status = column![
            choices,
            text(encoder).size(style::SMALL).style(shell::muted),
            text("Tabs here show the source frames: no encode or decode in the way").size(style::SMALL).style(shell::muted),
        ]
        .spacing(4);
        if self.video.hardware.len() > 1 {
            status = status.push(
                text(format!("Hardware encoders: {}", self.video.hardware.join(", "))).size(style::SMALL).style(shell::muted),
            );
        }
        status.into()
    }
}

/// Server name of RFB surfaces, listed under the VM form.
const VM_SERVER: &str = "vm";

fn available_rows<'a>(available: &[&'a Available]) -> Element<'a, Message> {
    let mut listed = Column::new().spacing(1);
    for available in available {
        let info = &available.info;
        let size = if info.width > 0 { format!("{}×{}", info.width, info.height) } else { info.mode.as_str().to_owned() };
        let row = row![
            text(&info.name).size(style::TEXT),
            container(text(&available.server).size(style::SMALL)).padding([1, 6]).style(shell::badge),
            space::horizontal(),
            text(size).size(style::SMALL).style(shell::muted),
        ]
        .spacing(8)
        .align_y(Center);
        listed = listed.push(
            button(row).on_press(Message::OpenAvailable(info.uri.clone())).width(Fill).padding([5, 8]).style(shell::row),
        );
    }
    listed.into()
}

fn section<'a>(icon: Icon, title: &'a str, body: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![
        row![muted(icon, 14.0), text(title).size(style::TEXT)].spacing(8).align_y(Center),
        body.into(),
    ]
    .spacing(8)
    .into()
}

/// How the launcher names the browser: Playwright's builds by their kind,
/// others by their executable name.
fn browser_name(executable: &std::path::Path) -> String {
    let path = executable.to_string_lossy();
    if path.contains("ms-playwright/chromium_headless_shell") {
        return "Chromium headless shell (Playwright)".to_owned();
    }
    if path.contains("ms-playwright/chromium") {
        return "Chromium (Playwright)".to_owned();
    }
    executable.file_name().map_or_else(|| path.into_owned(), |n| n.to_string_lossy().into_owned())
}

fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut short: String = text.chars().take(max.saturating_sub(1)).collect();
    short.push('…');
    short
}
