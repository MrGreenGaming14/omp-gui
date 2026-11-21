//! Pixel mode: frames stream through the decoder chain onto a retained framebuffer, and
//! pointer, wheel and keys go back in surface coordinates, stamped for the latency meter.

use super::{Clock, Event, Message, Notice, PointerInput, SurfaceMode, TabCtx};
use crate::decode::{Decoders, encoding_name, is_video};
use crate::framebuffer::Framebuffer;
use crate::geometry::{Placement, fit};
use crate::latency::{LatencyMeter, Reading};
use crate::panel::Options;
use crate::theme;
use crate::viewer::Viewer;
use futures::Stream;
use iced::widget::{canvas, image::Handle};
use iced::{Element, Fill, Point, Size, Task};
use loa_contracts::surface::{Encoding, Frame, Input, InputEvent, Modifiers, Rect, StreamInfo, Surface};
use loa_surfaces::pixels::STILL_ENCODINGS;
use loa_ui_kit::Tone;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

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

pub struct PixelMode {
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
    decoders: Decoders,
    /// The surface size the info gives, until the first frame sets the framebuffer's.
    size: (u32, u32),
    fb: Option<Framebuffer>,
    picture: Option<Handle>,
    /// Newest frame taken.
    last_seq: Option<u64>,
    last_encoding: Option<Encoding>,
    awaiting_keyframe: bool,
    asked_for_still: Option<Encoding>,
}

impl PixelMode {
    pub(super) fn open(tab: &mut TabCtx, options: &Options) -> (Self, Task<Message>) {
        let (input, inputs) = mpsc::unbounded_channel();
        let decoders = Decoders::new(options.decoder.as_ref().map(|make| make()));
        let mut work = vec![Task::stream(deliver_input(tab.surface.clone(), inputs))];
        match tab.surface.frames() {
            // A late viewer gets the current picture now instead of waiting for the next change.
            Ok(frames) => {
                work.extend([Task::stream(frame_stream(frames)), request_keyframe(&tab.surface), negotiate(&tab.surface, decoders.accepted())])
            }
            Err(e) => tab.notice = Some(Notice::Error(e.to_string())),
        }
        let mode = Self {
            clock: options.clock.clone(),
            input,
            next_seq: 1,
            focused: false,
            modifiers: Modifiers::default(),
            held_keys: Vec::new(),
            buttons_held: 0,
            cursor: None,
            panel: Size::ZERO,
            meter: LatencyMeter::default(),
            decoders,
            size: (tab.info.width, tab.info.height),
            fb: None,
            picture: None,
            last_seq: None,
            last_encoding: None,
            awaiting_keyframe: false,
            asked_for_still: None,
        };
        (mode, Task::batch(work))
    }

    /// The current picture.
    pub fn framebuffer(&self) -> Option<&Framebuffer> {
        self.fb.as_ref()
    }

    /// Sequence number of the newest frame taken.
    pub fn shown_seq(&self) -> Option<u64> {
        self.last_seq
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

    /// Where the surface sits in the panel, as last measured.
    pub fn placement(&self) -> Option<Placement> {
        self.placement_in(self.panel)
    }

    /// Where the surface sits in a panel of size `panel`.
    pub fn placement_in(&self, panel: Size) -> Option<Placement> {
        let (width, height) = self.fb.as_ref().map_or(self.size, |fb| (fb.width(), fb.height()));
        fit(width, height, panel)
    }

    pub(crate) fn panel(&self) -> Size {
        self.panel
    }

    pub(crate) fn picture(&self) -> Option<&Handle> {
        self.picture.as_ref()
    }

    /// The encoding of the last frame shown, as a stream.
    pub(super) fn last_stream(&self) -> Option<StreamInfo> {
        self.last_encoding.map(|encoding| StreamInfo { encoding, encoder: String::new(), hardware: false })
    }

    /// Stamps and queues one input. `measured` inputs feed the latency meter.
    fn send(&mut self, tab: &mut TabCtx, input: Input, measured: bool) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let sent_us = self.clock.now_us();
        if measured {
            self.meter.input_sent(seq, sent_us);
        }
        if self.input.send(InputEvent { seq, sent_us, input }).is_err() {
            tab.notice = Some(Notice::Closed);
        }
    }

    fn on_panel(&mut self, size: Size, tab: &mut TabCtx) {
        if size == self.panel {
            return;
        }
        self.panel = size;
        let (width, height) = (size.width.round() as u32, size.height.round() as u32);
        if width > 0 && height > 0 {
            self.send(tab, Input::Resize { width, height }, false);
        }
    }

    fn on_pointer(&mut self, input: PointerInput, tab: &mut TabCtx) {
        let placement = self.placement();
        match input {
            PointerInput::Moved(at) => {
                let dragging = self.buttons_held > 0;
                let on_surface = placement.and_then(|p| if dragging { Some(p.to_surface_clamped(at)) } else { p.to_surface(at) });
                self.cursor = Some(LocalCursor { at, on_surface, pressed: dragging });
                if let Some(p) = on_surface {
                    self.send(tab, Input::MouseMove { x: p.x, y: p.y }, false);
                }
            }
            PointerInput::Left => {
                if self.buttons_held == 0 {
                    self.cursor = None;
                }
            }
            PointerInput::Button { at, button, pressed } => {
                self.focused |= pressed;
                // A press must land on the picture; a release pins to its edge.
                let target = placement.and_then(|p| if pressed { p.to_surface(at) } else { Some(p.to_surface_clamped(at)) });
                let Some(target) = target else { return };
                if pressed {
                    self.buttons_held += 1;
                } else if self.buttons_held == 0 {
                    return;
                } else {
                    self.buttons_held -= 1;
                }
                self.cursor = Some(LocalCursor { at, on_surface: Some(target), pressed: self.buttons_held > 0 });
                let input = Input::MouseButton { x: target.x, y: target.y, button, pressed, modifiers: self.modifiers };
                self.send(tab, input, pressed);
            }
            PointerInput::Scroll { at, dx, dy } => {
                let Some((p, target)) = placement.and_then(|p| Some((p, p.to_surface(at)?))) else { return };
                let (dx, dy) = (p.to_surface_len(dx), p.to_surface_len(dy));
                self.send(tab, Input::Scroll { x: target.x, y: target.y, dx, dy, modifiers: self.modifiers }, true);
            }
        }
    }

    fn on_frame(&mut self, frame: &Frame, tab: &mut TabCtx) -> (Task<Message>, Option<Event>) {
        let none = || (Task::none(), None);
        if self.last_seq.is_some_and(|last| frame.seq <= last) {
            return none();
        }
        let video = is_video(frame.encoding);
        if self.decoders.find(frame.encoding).is_none() {
            tab.notice = Some(Notice::Unsupported(frame.encoding));
            if self.asked_for_still.replace(frame.encoding) == Some(frame.encoding) {
                return none();
            }
            // Ask the surface for stills itself; the app hears about it too.
            let event = Event::EncodingUnsupported { uri: tab.info.uri.clone(), encoding: frame.encoding, supported: STILL_ENCODINGS.to_vec() };
            return (negotiate(&tab.surface, STILL_ENCODINGS.to_vec()), Some(event));
        }
        if video && self.awaiting_keyframe && !frame.keyframe {
            return none();
        }
        let mut task = Task::none();
        if self.fb.as_ref().is_none_or(|fb| (fb.width(), fb.height()) != (frame.width, frame.height)) {
            let Some(fb) = Framebuffer::new(frame.width, frame.height) else {
                tab.notice = Some(Notice::Error(format!("a {}x{} frame is too large", frame.width, frame.height)));
                return none();
            };
            self.fb = Some(fb);
            let full = frame.region == Rect { x: 0, y: 0, width: frame.width, height: frame.height };
            if !full && !frame.keyframe {
                task = request_keyframe(&tab.surface);
            }
        }
        let (Some(fb), Some(decoder)) = (self.fb.as_mut(), self.decoders.find(frame.encoding)) else { return (task, None) };
        // Hand the pixels back from the renderer before writing.
        self.picture = None;
        match decoder.apply(frame, fb) {
            Ok(shown) => {
                self.last_seq = Some(frame.seq);
                self.last_encoding = Some(frame.encoding);
                self.awaiting_keyframe = false;
                if shown {
                    self.meter.frame_shown(frame.input_seq, self.clock.now_us());
                    if matches!(tab.notice, Some(Notice::Unsupported(_) | Notice::Error(_))) {
                        tab.notice = None;
                    }
                }
            }
            Err(e) => {
                tab.notice = Some(Notice::Error(e.to_string()));
                if video {
                    decoder.reset();
                    self.awaiting_keyframe = true;
                    task = request_keyframe(&tab.surface);
                }
            }
        }
        if self.last_seq.is_some() {
            self.picture = Some(fb.handle());
        }
        (task, None)
    }

    /// Frames were lost: region updates and video references are stale, so a keyframe is needed.
    fn on_lag(&mut self, tab: &TabCtx) -> Task<Message> {
        if self.last_encoding.is_some_and(is_video) {
            self.awaiting_keyframe = true;
            self.decoders.reset();
        }
        request_keyframe(&tab.surface)
    }
}

impl SurfaceMode for PixelMode {
    fn update(&mut self, message: Message, tab: &mut TabCtx) -> (Task<Message>, Option<Event>) {
        match message {
            Message::Frame(frame) => return self.on_frame(&frame, tab),
            Message::Lagged(_) => return (self.on_lag(tab), None),
            Message::Panel(size) => self.on_panel(size, tab),
            Message::Pointer { panel, input } => {
                self.on_panel(panel, tab);
                self.on_pointer(input, tab);
            }
            Message::Modifiers(modifiers) => self.modifiers = modifiers,
            Message::Key(key) => {
                if self.focused
                    && let Some(input) = key.to_input(&mut self.held_keys)
                {
                    self.send(tab, input, key.pressed);
                }
            }
            Message::Blur => self.focused = false,
            _ => {}
        }
        (Task::none(), None)
    }

    fn header<'a>(&'a self, tab: &'a TabCtx) -> Vec<Element<'a, Message>> {
        let reading = self.meter.reading();
        let tone = if reading.is_some_and(|r| r.warning) { Tone::Warn } else { Tone::Faint };
        let latency = theme::chip(format!("Latency {}", latency_label(reading)), tone);
        let stream = tab.surface.stream().or_else(|| self.last_stream());
        stream.map(|s| theme::chip(stream_label(&s), Tone::Faint)).into_iter().chain([latency]).collect()
    }

    fn view(&self) -> Element<'_, Message> {
        canvas(Viewer(self)).width(Fill).height(Fill).into()
    }

    fn pixel(&self) -> Option<&PixelMode> {
        Some(self)
    }
}

/// The latency meter text.
pub fn latency_label(reading: Option<Reading>) -> String {
    match reading {
        Some(r) => format!("{} ms, p95 {} ms", r.median_us.div_ceil(1000), r.p95_us.div_ceil(1000)),
        None => "-- ms".to_owned(),
    }
}

/// What the header says a pixel surface streams: the codec and its encoder for video, the
/// encoding alone for frames passed on as made.
pub fn stream_label(stream: &StreamInfo) -> String {
    let encoding = encoding_name(stream.encoding);
    if !is_video(stream.encoding) || stream.encoder.is_empty() {
        return encoding.to_owned();
    }
    let place = if stream.hardware { "hardware" } else { "software" };
    format!("{encoding} · {} · {place}", stream.encoder)
}

/// A surface call as a task reporting whether it went through.
fn call<F: Future<Output = loa_contracts::surface::Result<T>> + Send + 'static, T>(f: F) -> Task<Message> {
    Task::future(async move { f.await.map_or_else(|e| Message::Failed(e.to_string()), |_| Message::Done) })
}

/// Tells the surface what this renderer decodes; it may switch encodings.
fn negotiate(surface: &Arc<dyn Surface>, accepted: Vec<Encoding>) -> Task<Message> {
    let surface = surface.clone();
    call(async move { surface.negotiate(&accepted).await })
}

fn request_keyframe(surface: &Arc<dyn Surface>) -> Task<Message> {
    let surface = surface.clone();
    call(async move { surface.request_keyframe().await })
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

/// Sends queued inputs to the surface one at a time, in order. Yields a message only when a
/// send fails.
fn deliver_input(surface: Arc<dyn Surface>, inputs: mpsc::UnboundedReceiver<InputEvent>) -> impl Stream<Item = Message> {
    futures::stream::unfold((surface, inputs), |(surface, mut inputs)| async move {
        loop {
            let event = inputs.recv().await?;
            if let Err(e) = surface.input(event).await {
                return Some((Message::Failed(e.to_string()), (surface, inputs)));
            }
        }
    })
}
