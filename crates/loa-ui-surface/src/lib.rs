//! Surface renderer: the right panel view of any [`loa_view::surface::Surface`]. Pixel
//! surfaces stream frames onto a retained framebuffer and take input back in surface
//! coordinates; document surfaces are fetched once and rendered here through the app's
//! [`loa_view::surface::port::DocumentRenderer`].

pub mod decode;
pub mod framebuffer;
pub mod geometry;
pub mod keys;
pub mod latency;
pub mod panel;
pub mod picker;
mod tab;
mod theme;
#[cfg(feature = "remote-video")]
mod video;
mod viewer;

pub use framebuffer::Framebuffer;
pub use geometry::{Placement, fit};
pub use keys::KeyInput;
pub use latency::{LatencyMeter, Reading};
pub use panel::{Options, Panel};
pub use tab::{
    Clock, DocumentMode, LocalCursor, Message, MonotonicClock, Pages, PixelMode, PointerInput, State, Tab, latency_label, stream_label,
};
pub use theme::SurfaceKindStyle;
