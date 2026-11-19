//! The surface tab component: state, messages, update and view.

use crate::decode::{STILL_ENCODINGS, VideoDecoder, apply_still, encoding_name, is_video};
use crate::document::{
    MAX_ZOOM, MIN_ZOOM, PAGE_GAP, PageError, PageRenderers, Pages, ZOOM_STEP, image_size,
};
use crate::framebuffer::Framebuffer;
use crate::geometry::{Placement, fit};
use crate::input::KeyInput;
use crate::latency::{LatencyMeter, Reading};
use crate::style;
use crate::viewer::Viewer;
use loa_ui_shell::theme::style as shell;
use futures::Stream;
use iced::widget::image::Handle;
use iced::widget::{button, canvas, column, container, image, row, scrollable, space, text};
use iced::{Center, Element, Fill, Point, Size, Task};
use loa_surface::{
    Document, Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Surface, SurfaceInfo,
    SurfaceMode, SurfaceUri,
};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{broadcast, mpsc};

/// The viewer clock that stamps inputs and frame arrivals.
pub trait Clock: Send + Sync {
    /// Microseconds since an arbitrary fixed point.
    fn now_us(&self) -> u64;
}

/// [`Clock`] counting from its creation on the monotonic system clock.
#[derive(Debug)]
pub struct MonotonicClock {
    start: Instant,
}

impl Default for MonotonicClock {
    fn default() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Clock for MonotonicClock {
    fn now_us(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

/// What the app plugs into a surface tab.
pub struct Options {
    pub clock: Arc<dyn Clock>,
    /// Decodes H.264 and HEVC. Without one, video surfaces are asked for a
    /// still encoding.
    pub video: Option<Box<dyn VideoDecoder>>,
    /// Renders documents that aren't images.
    pub documents: Option<Arc<dyn PageRenderers>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            clock: Arc::new(MonotonicClock::default()),
            video: None,
            documents: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Frame(Arc<Frame>),
    /// The frame stream skipped this many frames.
    Lagged(u64),
    StreamEnded,
    /// The canvas has a new size.
    Panel(Size),
    /// Pointer input in panel coordinates, with the panel size it was seen at.
    Pointer {
        panel: Size,
        input: PointerInput,
    },
    Modifiers(Modifiers),
    Key(KeyInput),
    /// A click landed outside the surface.
    Blur,
    Document(Result<Arc<Document>, String>),
    /// The document view scrolled: `top` of the viewport in content pixels.
    DocumentScrolled {
        top: f32,
        viewport: Size,
    },
    ZoomIn,
    ZoomOut,
    ZoomReset,
    /// A call to the surface failed.
    Failed(String),
    /// A call to the surface went through.
    Done,
}

/// Pointer input in panel coordinates (pixels from the canvas's top left).
#[derive(Clone, Debug, PartialEq)]
pub enum PointerInput {
    Moved(Point),
    Left,
    Button {
        at: Point,
        button: MouseButton,
        pressed: bool,
    },
    /// Panel pixels, positive `dy` scrolls the content down.
    Scroll {
        at: Point,
        dx: f32,
        dy: f32,
    },
}

/// What the app must act on.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The surface streams `encoding`, which this renderer can't decode. The
    /// app asks the server to switch to one of `supported`.
    EncodingUnsupported {
        uri: SurfaceUri,
        encoding: Encoding,
        supported: Vec<Encoding>,
    },
}

/// The locally drawn pointer.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LocalCursor {
    /// Panel coordinates.
    pub at: Point,
    /// The surface point under it, `None` over the bars around the picture.
    pub on_surface: Option<Point>,
    /// A button is held.
    pub pressed: bool,
}

/// What the tab header shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub title: String,
    pub notice: Option<String>,
    pub latency: Option<Reading>,
}

#[derive(Clone, Debug, PartialEq)]
enum Notice {
    Unsupported(Encoding),
    Error(String),
    Closed,
}

impl Notice {
    fn text(&self) -> String {
        match self {
            Notice::Unsupported(encoding) => {
                format!(
                    "{} video can't be shown here, asked for PNG, JPEG or RGBA",
                    encoding_name(*encoding)
                )
            }
            Notice::Error(message) => message.clone(),
            Notice::Closed => "The surface closed".to_owned(),
        }
    }
}

struct PixelView {
    fb: Option<Framebuffer>,
    picture: Option<Handle>,
    /// Newest frame taken.
    last_seq: Option<u64>,
    last_encoding: Option<Encoding>,
    video: Option<Box<dyn VideoDecoder>>,
    awaiting_keyframe: bool,
    asked_for_still: Option<Encoding>,
}

enum DocumentView {
    Loading,
    Image { handle: Handle, size: Size },
    Pages(Pages),
    Failed(String),
}

struct DocumentState {
    view: DocumentView,
    renderers: Option<Arc<dyn PageRenderers>>,
    zoom: f32,
    top: f32,
    viewport: Size,
}

enum Body {
    Pixel(PixelView),
    Document(DocumentState),
}

/// One surface open in the right panel.
pub struct State {
    info: SurfaceInfo,
    surface: Arc<dyn Surface>,
    clock: Arc<dyn Clock>,
    input: mpsc::UnboundedSender<InputEvent>,
    next_seq: u64,
    focused: bool,
    modifiers: Modifiers,
    held_keys: Vec<String>,
    buttons_held: u32,
    cursor: Option<LocalCursor>,
    panel: Size,
    meter: LatencyMeter,
    body: Body,
    notice: Option<Notice>,
    _tasks: iced::task::Handle,
}

impl State {
    /// Opens `surface`. The returned task streams frames (pixel mode) or
    /// fetches the file (document mode) and delivers input in order; it
    /// stops when the state is dropped.
    pub fn open(surface: Arc<dyn Surface>, options: Options) -> (Self, Task<Message>) {
        let info = surface.info();
        let (input, inputs) = mpsc::unbounded_channel();
        let mut notice = None;
        let (body, work) = match info.mode {
            SurfaceMode::Pixel => {
                let mut work = vec![Task::stream(deliver_input(surface.clone(), inputs))];
                match surface.frames() {
                    // A late viewer gets the current picture now instead of
                    // waiting for the next change on screen.
                    Ok(frames) => {
                        work.push(Task::stream(frame_stream(frames)));
                        work.push(request_keyframe(&surface));
                    }
                    Err(e) => notice = Some(Notice::Error(e.to_string())),
                }
                let view = PixelView {
                    fb: None,
                    picture: None,
                    last_seq: None,
                    last_encoding: None,
                    video: options.video,
                    awaiting_keyframe: false,
                    asked_for_still: None,
                };
                (Body::Pixel(view), work)
            }
            SurfaceMode::Document => {
                let source = surface.clone();
                let fetch = Task::future(async move {
                    Message::Document(
                        source
                            .document()
                            .await
                            .map(Arc::new)
                            .map_err(|e| e.to_string()),
                    )
                });
                let view = DocumentState {
                    view: DocumentView::Loading,
                    renderers: options.documents,
                    zoom: 1.0,
                    top: 0.0,
                    viewport: Size::new(info.width as f32, info.height as f32),
                };
                (Body::Document(view), vec![fetch])
            }
        };
        let (task, handle) = Task::batch(work).abortable();
        let state = Self {
            info,
            surface,
            clock: options.clock,
            input,
            next_seq: 1,
            focused: false,
            modifiers: Modifiers::default(),
            held_keys: Vec::new(),
            buttons_held: 0,
            cursor: None,
            panel: Size::ZERO,
            meter: LatencyMeter::default(),
            body,
            notice,
            _tasks: handle.abort_on_drop(),
        };
        (state, task)
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Frame(frame) => return self.on_frame(&frame),
            Message::Lagged(_) => return (self.on_lag(), None),
            Message::StreamEnded => self.notice = Some(Notice::Closed),
            Message::Panel(size) => self.on_panel(size),
            Message::Pointer { panel, input } => {
                self.on_panel(panel);
                self.on_pointer(input);
            }
            Message::Modifiers(modifiers) => self.modifiers = modifiers,
            Message::Key(key) => {
                if self.focused
                    && let Some(input) = key.to_input(&mut self.held_keys)
                {
                    let measured = key.pressed;
                    self.send(input, measured);
                }
            }
            Message::Blur => self.focused = false,
            Message::Document(document) => self.on_document(document),
            Message::DocumentScrolled { top, viewport } => {
                if let Body::Document(doc) = &mut self.body {
                    doc.top = top;
                    doc.viewport = viewport;
                }
                self.refresh_pages();
            }
            Message::ZoomIn => self.zoom(|z| z * ZOOM_STEP),
            Message::ZoomOut => self.zoom(|z| z / ZOOM_STEP),
            Message::ZoomReset => self.zoom(|_| 1.0),
            Message::Failed(message) => self.notice = Some(Notice::Error(message)),
            Message::Done => {}
        }
        (Task::none(), None)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let mut header = row![text(&self.info.name).size(style::TEXT)]
            .spacing(8)
            .padding([6, 10])
            .align_y(Center);
        if let Some(notice) = &self.notice {
            header = header.push(
                text(notice.text())
                    .size(style::SMALL)
                    .style(style::warning),
            );
        }
        header = header.push(space::horizontal());
        let body: Element<'_, Message> = match &self.body {
            Body::Pixel(_) => {
                let latency = self.meter.reading();
                let warning = latency.is_some_and(|r| r.warning);
                header = header.push(
                    container(
                        text(format!("Latency {}", latency_label(latency))).size(style::SMALL),
                    )
                    .padding([2, 6])
                    .style(style::latency(warning)),
                );
                canvas(Viewer { state: self })
                    .width(Fill)
                    .height(Fill)
                    .into()
            }
            Body::Document(doc) => {
                let zoom = |label: String, message: Message| {
                    button(text(label).size(style::TEXT))
                        .on_press(message)
                        .padding([2, 8])
                        .style(shell::row)
                };
                header = header.push(
                    row![
                        zoom("−".into(), Message::ZoomOut),
                        zoom(format!("{:.0}%", doc.zoom * 100.0), Message::ZoomReset),
                        zoom("+".into(), Message::ZoomIn),
                    ]
                    .spacing(2)
                    .align_y(Center),
                );
                document_body(doc)
            }
        };
        column![header, body].into()
    }

    /// What the tab header shows: name, problems, latency.
    pub fn tab(&self) -> Tab {
        Tab {
            title: self.info.name.clone(),
            notice: self.notice(),
            latency: self.latency(),
        }
    }

    pub fn info(&self) -> &SurfaceInfo {
        &self.info
    }

    /// The pointer as drawn locally, independent of frames.
    pub fn cursor(&self) -> Option<LocalCursor> {
        self.cursor
    }

    /// Whether keys go to the surface.
    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn latency(&self) -> Option<Reading> {
        self.meter.reading()
    }

    pub fn notice(&self) -> Option<String> {
        self.notice.as_ref().map(Notice::text)
    }

    /// Sequence number of the newest frame taken.
    pub fn shown_seq(&self) -> Option<u64> {
        match &self.body {
            Body::Pixel(px) => px.last_seq,
            Body::Document(_) => None,
        }
    }

    /// The current picture of a pixel surface.
    pub fn framebuffer(&self) -> Option<&Framebuffer> {
        match &self.body {
            Body::Pixel(px) => px.fb.as_ref(),
            Body::Document(_) => None,
        }
    }

    /// Document mode: pages, `None` until loaded or for images.
    pub fn pages(&self) -> Option<&Pages> {
        match &self.body {
            Body::Document(DocumentState {
                view: DocumentView::Pages(pages),
                ..
            }) => Some(pages),
            _ => None,
        }
    }

    /// Document mode: pages in the loaded document, 1 for an image, `None`
    /// while loading or when it couldn't be opened.
    pub fn page_count(&self) -> Option<usize> {
        match &self.body {
            Body::Document(DocumentState {
                view: DocumentView::Image { .. },
                ..
            }) => Some(1),
            Body::Document(DocumentState {
                view: DocumentView::Pages(pages),
                ..
            }) => Some(pages.sizes().len()),
            _ => None,
        }
    }

    pub(crate) fn panel(&self) -> Size {
        self.panel
    }

    pub(crate) fn picture(&self) -> Option<&Handle> {
        match &self.body {
            Body::Pixel(px) => px.picture.as_ref(),
            Body::Document(_) => None,
        }
    }

    /// Where the surface sits in the panel, as last measured.
    pub fn placement(&self) -> Option<Placement> {
        self.placement_in(self.panel)
    }

    /// Where the surface sits in a panel of size `panel`.
    pub fn placement_in(&self, panel: Size) -> Option<Placement> {
        let (width, height) = match self.framebuffer() {
            Some(fb) => (fb.width(), fb.height()),
            None => (self.info.width, self.info.height),
        };
        fit(width, height, panel)
    }

    /// Stamps and queues one input. `measured` inputs feed the latency meter.
    fn send(&mut self, input: Input, measured: bool) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let sent_us = self.clock.now_us();
        if measured {
            self.meter.input_sent(seq, sent_us);
        }
        if self
            .input
            .send(InputEvent {
                seq,
                sent_us,
                input,
            })
            .is_err()
        {
            self.notice = Some(Notice::Closed);
        }
    }

    fn on_panel(&mut self, size: Size) {
        if size == self.panel {
            return;
        }
        self.panel = size;
        let (width, height) = (size.width.round() as u32, size.height.round() as u32);
        if matches!(self.body, Body::Pixel(_)) && width > 0 && height > 0 {
            self.send(Input::Resize { width, height }, false);
        }
    }

    fn on_pointer(&mut self, input: PointerInput) {
        if !matches!(self.body, Body::Pixel(_)) {
            return;
        }
        let placement = self.placement();
        match input {
            PointerInput::Moved(at) => {
                let dragging = self.buttons_held > 0;
                let on_surface = placement.and_then(|p| {
                    if dragging {
                        Some(p.to_surface_clamped(at))
                    } else {
                        p.to_surface(at)
                    }
                });
                self.cursor = Some(LocalCursor {
                    at,
                    on_surface,
                    pressed: dragging,
                });
                if let Some(p) = on_surface {
                    self.send(Input::MouseMove { x: p.x, y: p.y }, false);
                }
            }
            PointerInput::Left => {
                if self.buttons_held == 0 {
                    self.cursor = None;
                }
            }
            PointerInput::Button {
                at,
                button,
                pressed,
            } => {
                if pressed {
                    self.focused = true;
                }
                let target = placement.and_then(|p| {
                    if pressed {
                        p.to_surface(at)
                    } else {
                        Some(p.to_surface_clamped(at))
                    }
                });
                let Some(target) = target else { return };
                if pressed {
                    self.buttons_held += 1;
                } else if self.buttons_held == 0 {
                    return;
                } else {
                    self.buttons_held -= 1;
                }
                self.cursor = Some(LocalCursor {
                    at,
                    on_surface: Some(target),
                    pressed: self.buttons_held > 0,
                });
                let modifiers = self.modifiers;
                self.send(
                    Input::MouseButton {
                        x: target.x,
                        y: target.y,
                        button,
                        pressed,
                        modifiers,
                    },
                    pressed,
                );
            }
            PointerInput::Scroll { at, dx, dy } => {
                let Some(p) = placement else { return };
                let Some(target) = p.to_surface(at) else {
                    return;
                };
                let (dx, dy) = (p.to_surface_len(dx), p.to_surface_len(dy));
                let modifiers = self.modifiers;
                self.send(
                    Input::Scroll {
                        x: target.x,
                        y: target.y,
                        dx,
                        dy,
                        modifiers,
                    },
                    true,
                );
            }
        }
    }

    fn on_frame(&mut self, frame: &Frame) -> (Task<Message>, Option<Event>) {
        let Body::Pixel(px) = &mut self.body else {
            return (Task::none(), None);
        };
        if px.last_seq.is_some_and(|last| frame.seq <= last) {
            return (Task::none(), None);
        }
        let video = is_video(frame.encoding);
        if video
            && !px
                .video
                .as_ref()
                .is_some_and(|d| d.supports(frame.encoding))
        {
            self.notice = Some(Notice::Unsupported(frame.encoding));
            if px.asked_for_still == Some(frame.encoding) {
                return (Task::none(), None);
            }
            px.asked_for_still = Some(frame.encoding);
            let event = Event::EncodingUnsupported {
                uri: self.info.uri.clone(),
                encoding: frame.encoding,
                supported: STILL_ENCODINGS.to_vec(),
            };
            return (Task::none(), Some(event));
        }
        if video && px.awaiting_keyframe && !frame.keyframe {
            return (Task::none(), None);
        }
        let mut task = Task::none();
        let full = frame.region.x == 0
            && frame.region.y == 0
            && frame.region.width == frame.width
            && frame.region.height == frame.height;
        if px
            .fb
            .as_ref()
            .is_none_or(|fb| (fb.width(), fb.height()) != (frame.width, frame.height))
        {
            let Some(fb) = Framebuffer::new(frame.width, frame.height) else {
                self.notice = Some(Notice::Error(format!(
                    "a {}x{} frame is too large",
                    frame.width, frame.height
                )));
                return (Task::none(), None);
            };
            px.fb = Some(fb);
            px.picture = None;
            if !full && !frame.keyframe {
                task = request_keyframe(&self.surface);
            }
        }
        let Some(fb) = px.fb.as_mut() else {
            return (task, None);
        };
        // Hand the pixels back from the renderer before writing.
        px.picture = None;
        let result = match (&mut px.video, video) {
            (Some(decoder), true) => decoder.decode(frame, fb.pixels_mut()),
            _ => apply_still(fb, frame).map(|()| true),
        };
        match result {
            Ok(shown) => {
                px.last_seq = Some(frame.seq);
                px.last_encoding = Some(frame.encoding);
                px.awaiting_keyframe = false;
                if shown {
                    self.meter.frame_shown(frame.input_seq, self.clock.now_us());
                    if matches!(self.notice, Some(Notice::Unsupported(_) | Notice::Error(_))) {
                        self.notice = None;
                    }
                }
            }
            Err(e) => {
                self.notice = Some(Notice::Error(e.to_string()));
                if video && let Some(decoder) = &mut px.video {
                    decoder.reset();
                    px.awaiting_keyframe = true;
                    task = request_keyframe(&self.surface);
                }
            }
        }
        if px.last_seq.is_some() {
            px.picture = Some(fb.handle());
        }
        (task, None)
    }

    /// Frames were lost: region updates and video references are stale, so
    /// a keyframe is needed.
    fn on_lag(&mut self) -> Task<Message> {
        let Body::Pixel(px) = &mut self.body else {
            return Task::none();
        };
        if px.last_encoding.is_some_and(is_video) {
            px.awaiting_keyframe = true;
            if let Some(decoder) = &mut px.video {
                decoder.reset();
            }
        }
        request_keyframe(&self.surface)
    }

    fn on_document(&mut self, document: Result<Arc<Document>, String>) {
        let Body::Document(doc) = &mut self.body else {
            return;
        };
        let opened = match document {
            Err(e) => Err(e),
            Ok(document) if document.mime.starts_with("image/") => image_size(&document.data)
                .map(|size| DocumentView::Image {
                    handle: Handle::from_bytes(document.data.clone()),
                    size,
                })
                .map_err(|e| e.to_string()),
            Ok(document) => match &doc.renderers {
                Some(renderers) => renderers
                    .open(&document)
                    .and_then(Pages::new)
                    .map(DocumentView::Pages),
                None => Err(PageError::Unsupported(document.mime.clone())),
            }
            .map_err(|e| e.to_string()),
        };
        doc.view = match opened {
            Ok(view) => view,
            Err(e) => {
                self.notice = Some(Notice::Error(e.clone()));
                DocumentView::Failed(e)
            }
        };
        self.refresh_pages();
    }

    fn zoom(&mut self, change: impl FnOnce(f32) -> f32) {
        if let Body::Document(doc) = &mut self.body {
            doc.zoom = change(doc.zoom).clamp(MIN_ZOOM, MAX_ZOOM);
        }
        self.refresh_pages();
    }

    fn refresh_pages(&mut self) {
        let Body::Document(doc) = &mut self.body else {
            return;
        };
        if let DocumentView::Pages(pages) = &mut doc.view
            && let Err(e) = pages.refresh(doc.zoom, doc.top, doc.top + doc.viewport.height)
        {
            self.notice = Some(Notice::Error(e.to_string()));
        }
    }
}

/// The latency meter text.
pub fn latency_label(reading: Option<Reading>) -> String {
    match reading {
        Some(r) => format!(
            "{} ms, p95 {} ms",
            r.median_us.div_ceil(1000),
            r.p95_us.div_ceil(1000)
        ),
        None => "-- ms".to_owned(),
    }
}

fn document_body(doc: &DocumentState) -> Element<'_, Message> {
    let content: Element<'_, Message> = match &doc.view {
        DocumentView::Loading => return container(text("Loading")).center(Fill).into(),
        DocumentView::Failed(e) => {
            return container(text(e.as_str()).style(text::warning))
                .center(Fill)
                .into();
        }
        DocumentView::Image { handle, size } => image(handle.clone())
            .width(size.width * doc.zoom)
            .height(size.height * doc.zoom)
            .into(),
        DocumentView::Pages(pages) => {
            let items = pages.sizes().iter().enumerate().map(|(i, size)| {
                let (w, h) = (size.width * doc.zoom, size.height * doc.zoom);
                match pages.rendered(i) {
                    Some(r) => image(r.handle.clone()).width(w).height(h).into(),
                    None => container(space())
                        .width(w)
                        .height(h)
                        .style(container::bordered_box)
                        .into(),
                }
            });
            column(items).spacing(PAGE_GAP).into()
        }
    };
    scrollable(content)
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::default(),
            horizontal: scrollable::Scrollbar::default(),
        })
        .on_scroll(|v| Message::DocumentScrolled {
            top: v.absolute_offset().y,
            viewport: v.bounds().size(),
        })
        .width(Fill)
        .height(Fill)
        .into()
}

fn request_keyframe(surface: &Arc<dyn Surface>) -> Task<Message> {
    let surface = surface.clone();
    Task::future(async move {
        match surface.request_keyframe().await {
            Ok(()) => Message::Done,
            Err(e) => Message::Failed(e.to_string()),
        }
    })
}

/// Frames from the broadcast receiver as messages, ending when it closes.
fn frame_stream(frames: broadcast::Receiver<Arc<Frame>>) -> impl Stream<Item = Message> {
    futures::stream::unfold(Some(frames), |frames| async move {
        let mut frames = frames?;
        match frames.recv().await {
            Ok(frame) => Some((Message::Frame(frame), Some(frames))),
            Err(broadcast::error::RecvError::Lagged(n)) => Some((Message::Lagged(n), Some(frames))),
            Err(broadcast::error::RecvError::Closed) => Some((Message::StreamEnded, None)),
        }
    })
}

/// Sends queued inputs to the surface one at a time, in order. Yields a
/// message only when a send fails.
fn deliver_input(
    surface: Arc<dyn Surface>,
    inputs: mpsc::UnboundedReceiver<InputEvent>,
) -> impl Stream<Item = Message> {
    futures::stream::unfold((surface, inputs), |(surface, mut inputs)| async move {
        loop {
            let event = inputs.recv().await?;
            if let Err(e) = surface.input(event).await {
                return Some((Message::Failed(e.to_string()), (surface, inputs)));
            }
        }
    })
}
