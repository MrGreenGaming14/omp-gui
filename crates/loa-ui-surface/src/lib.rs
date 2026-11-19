//! Surface renderer: the generic right panel view of any [`loa_surface::Surface`].
//!
//! Pixel surfaces stream frames onto a retained framebuffer, scaled to the
//! panel with letterboxing, and get mouse, scroll and keys back in surface
//! coordinates. The cursor and hover ring are drawn locally on every pointer
//! move, and a latency meter pairs inputs with the frames that show them.
//! Document surfaces are fetched once and rendered here, so scroll and zoom
//! stay local.

pub mod decode;
mod docs;
pub mod document;
pub mod framebuffer;
pub mod geometry;
mod h264;
pub mod input;
pub mod latency;
pub mod panel;
pub mod picker;
mod state;
mod style;
mod viewer;

pub use decode::{DecodeError, VideoDecoder};
pub use docs::DocsRenderers;
pub use h264::H264Decoder;
pub use panel::{
    Available, BrowserStatus, Capture, CaptureStatus, NewDecoder, Panel, PanelOptions, VideoStatus,
};
pub use document::{PageError, PageRenderer, PageRenderers, Pages, RgbaPage};
pub use framebuffer::Framebuffer;
pub use geometry::{Placement, fit};
pub use input::KeyInput;
pub use latency::{LatencyMeter, Reading};
pub use state::{
    Clock, Event, LocalCursor, Message, MonotonicClock, Options, PointerInput, State, Tab,
    latency_label, stream_label,
};
