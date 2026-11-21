//! One surface open in the panel: the tab shell (name, notice, header) around its
//! [`SurfaceMode`] strategy, [`PixelMode`] for streamed frames or [`DocumentMode`] for files
//! rendered here.

mod document;
mod pixel;

pub use document::{DocumentMode, Pages};
pub use pixel::{LocalCursor, PixelMode, latency_label, stream_label};

use crate::decode::encoding_name;
use crate::keys::KeyInput;
use crate::latency::Reading;
use crate::panel::Options;
use crate::theme;
use iced::widget::{column, container, row, text};
use iced::{Center, Element, Fill, Point, Size, Task};
use loa_view::surface::{self, Document, Encoding, Frame, Intent, Modifiers, MouseButton, StreamInfo, Surface, SurfaceInfo};
use loa_ui_kit::theme::{SpaceRole as Sp, sp};
use std::sync::Arc;
use std::time::Instant;

/// The viewer clock that stamps inputs and frame arrivals.
pub trait Clock: Send + Sync {
    /// Microseconds since an arbitrary fixed point.
    fn now_us(&self) -> u64;
}

/// [`Clock`] counting from its creation on the monotonic system clock.
#[derive(Debug)]
pub struct MonotonicClock(Instant);

impl Default for MonotonicClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

impl Clock for MonotonicClock {
    fn now_us(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_micros()).unwrap_or(u64::MAX)
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
    Pointer { panel: Size, input: PointerInput },
    Modifiers(Modifiers),
    Key(KeyInput),
    /// A click landed outside the surface.
    Blur,
    Document(Result<Arc<Document>, String>),
    /// The document view scrolled: `top` of the viewport in content pixels.
    DocumentScrolled { top: f32, viewport: Size },
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
    Button { at: Point, button: MouseButton, pressed: bool },
    /// Panel pixels, positive `dy` scrolls the content down.
    Scroll { at: Point, dx: f32, dy: f32 },
}

/// What the tab header shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub title: String,
    pub notice: Option<String>,
    pub latency: Option<Reading>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Notice {
    Unsupported(Encoding),
    Error(String),
    Closed,
}

impl Notice {
    fn text(&self) -> String {
        match self {
            Notice::Unsupported(encoding) => format!("{} video can't be shown here, asked for PNG, JPEG or RGBA", encoding_name(*encoding)),
            Notice::Error(message) => message.clone(),
            Notice::Closed => "The surface closed".to_owned(),
        }
    }
}

/// What a mode sees of its tab.
pub(crate) struct TabCtx {
    pub info: SurfaceInfo,
    pub surface: Arc<dyn Surface>,
    pub notice: Option<Notice>,
}

/// The strategy of a tab for its surface's mode.
pub(crate) trait SurfaceMode: Send {
    /// Handles every message the shell doesn't; others' messages are ignored.
    fn update(&mut self, message: Message, tab: &mut TabCtx) -> (Task<Message>, Option<Intent>);

    /// Chips after the name and notice in the header.
    fn header<'a>(&'a self, tab: &'a TabCtx) -> Vec<Element<'a, Message>>;

    fn view(&self) -> Element<'_, Message>;

    fn pixel(&self) -> Option<&PixelMode> {
        None
    }

    fn document(&self) -> Option<&DocumentMode> {
        None
    }
}

/// One surface open in the right panel.
pub struct State {
    tab: TabCtx,
    mode: Box<dyn SurfaceMode>,
    _tasks: iced::task::Handle,
}

impl State {
    /// Opens `surface`. The returned task streams frames (pixel mode) or fetches the file
    /// (document mode) and delivers input in order; it stops when the state is dropped.
    pub fn open(surface: Arc<dyn Surface>, options: &Options) -> (Self, Task<Message>) {
        let mut tab = TabCtx { info: surface.info(), surface, notice: None };
        let (mode, work): (Box<dyn SurfaceMode>, _) = match tab.info.mode {
            surface::SurfaceMode::Pixel => {
                let (mode, work) = PixelMode::open(&mut tab, options);
                (Box::new(mode), work)
            }
            surface::SurfaceMode::Document => {
                let (mode, work) = DocumentMode::open(&tab, options);
                (Box::new(mode), work)
            }
        };
        let (task, handle) = work.abortable();
        (Self { tab, mode, _tasks: handle.abort_on_drop() }, task)
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Intent>) {
        match message {
            Message::StreamEnded => self.tab.notice = Some(Notice::Closed),
            Message::Failed(message) => self.tab.notice = Some(Notice::Error(message)),
            Message::Done => {}
            other => return self.mode.update(other, &mut self.tab),
        }
        (Task::none(), None)
    }

    pub fn view(&self) -> Element<'_, Message> {
        // Long names (a data: URL without a title) are clipped to one line so the chips keep
        // their width.
        let name = theme::body(&self.tab.info.name).wrapping(text::Wrapping::None);
        let notice = self.notice().map(|notice| Element::from(theme::warning(notice)));
        let header = row![container(name).width(Fill).clip(true)].push(notice);
        let header = header.extend(self.mode.header(&self.tab)).spacing(sp(Sp::S)).padding([sp(Sp::Xs), sp(Sp::M)]).align_y(Center);
        column![header, self.mode.view()].into()
    }

    /// What the tab header shows: name, problems, latency.
    pub fn tab(&self) -> Tab {
        Tab { title: self.tab.info.name.clone(), notice: self.notice(), latency: self.latency() }
    }

    pub fn info(&self) -> &SurfaceInfo {
        &self.tab.info
    }

    pub fn notice(&self) -> Option<String> {
        self.tab.notice.as_ref().map(Notice::text)
    }

    pub fn latency(&self) -> Option<Reading> {
        self.pixel().and_then(PixelMode::latency)
    }

    /// What the surface streams: what it says, or else the encoding of the last frame shown.
    pub fn stream(&self) -> Option<StreamInfo> {
        self.tab.surface.stream().or_else(|| self.pixel()?.last_stream())
    }

    /// The pixel mode, `None` for documents.
    pub fn pixel(&self) -> Option<&PixelMode> {
        self.mode.pixel()
    }

    /// The document mode, `None` for pixel surfaces.
    pub fn document(&self) -> Option<&DocumentMode> {
        self.mode.document()
    }
}
