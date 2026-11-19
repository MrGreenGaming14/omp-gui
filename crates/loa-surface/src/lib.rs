//! The surface contract: what a surface server offers and what the renderer
//! and peers consume.
//!
//! A surface is declared as a `surface://<server>/<name>` resource with a
//! name, a size and a mode. Pixel surfaces stream [`Frame`]s and take
//! [`InputEvent`]s in surface coordinates. Document surfaces hand over the
//! file once and the viewer renders it locally.

use async_trait::async_trait;
use bytes::Bytes;
pub use loa_core::SurfaceMode;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast;

/// ALPN of the frame and input stream.
pub const ALPN: &[u8] = b"omp/surface/1";

/// URI scheme of surface resources.
pub const SCHEME: &str = "surface";

#[derive(Debug, thiserror::Error)]
pub enum SurfaceError {
    #[error("`{0}` isn't a surface://<server>/<name> uri")]
    Uri(String),
    #[error("no surface {0}")]
    NotFound(SurfaceUri),
    #[error("{uri} is a {mode} surface")]
    Mode { uri: SurfaceUri, mode: SurfaceMode },
    #[error("the surface is closed")]
    Closed,
    #[error("{0}")]
    Backend(String),
}

pub type Result<T, E = SurfaceError> = std::result::Result<T, E>;

/// `surface://<server>/<name>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SurfaceUri(String);

impl SurfaceUri {
    pub fn new(server: &str, name: &str) -> Result<Self> {
        format!("{SCHEME}://{server}/{name}").parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parts(&self) -> (&str, &str) {
        // Checked in `from_str`.
        self.0[SCHEME.len() + 3..].split_once('/').unwrap_or_default()
    }

    pub fn server(&self) -> &str {
        self.parts().0
    }

    /// Everything after the server, e.g. `tab1`.
    pub fn name(&self) -> &str {
        self.parts().1
    }
}

impl std::str::FromStr for SurfaceUri {
    type Err = SurfaceError;

    fn from_str(text: &str) -> Result<Self> {
        let rest = text.strip_prefix(SCHEME).and_then(|r| r.strip_prefix("://"));
        match rest.and_then(|r| r.split_once('/')) {
            Some((server, name)) if !server.is_empty() && !name.is_empty() => Ok(Self(text.to_owned())),
            _ => Err(SurfaceError::Uri(text.to_owned())),
        }
    }
}

impl std::fmt::Display for SurfaceUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a server declares for each surface.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceInfo {
    pub uri: SurfaceUri,
    /// Shown on the tab.
    pub name: String,
    /// Size in surface pixels. For documents, the size of the first page.
    pub width: u32,
    pub height: u32,
    pub mode: SurfaceMode,
    /// Document mode: MIME type of the file.
    pub mime: Option<String>,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Encoding {
    /// 8 bit RGBA, row major, no padding.
    Rgba,
    Jpeg,
    Png,
    /// Annex B access unit.
    H264,
    /// Annex B access unit.
    Hevc,
}

/// One update of a pixel surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Counts up per surface. A frame older than the last one shown is dropped.
    pub seq: u64,
    /// Server clock when the frame was captured, in microseconds.
    pub captured_us: u64,
    /// Highest [`InputEvent::seq`] whose effect is in this frame. The latency
    /// meter pairs it with the time the input was sent.
    pub input_seq: u64,
    /// Full surface size at capture.
    pub width: u32,
    pub height: u32,
    /// Part of the surface `data` covers. Video encodings always cover it all.
    pub region: Rect,
    pub encoding: Encoding,
    /// Decodable without earlier frames.
    pub keyframe: bool,
    pub data: Bytes,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub meta: bool,
}

/// Input in surface coordinates (pixels from the top left).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Input {
    MouseMove { x: f32, y: f32 },
    MouseButton { x: f32, y: f32, button: MouseButton, pressed: bool, modifiers: Modifiers },
    /// `dx`, `dy` in pixels. Positive `dy` scrolls the content down.
    Scroll { x: f32, y: f32, dx: f32, dy: f32, modifiers: Modifiers },
    /// `key` is the W3C `KeyboardEvent.key` value, `code` the `KeyboardEvent.code`.
    Key { key: String, code: String, pressed: bool, modifiers: Modifiers },
    /// Committed text (typing, paste, IME).
    Text { text: String },
    /// The viewer's panel changed size. A server may resize the surface.
    Resize { width: u32, height: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputEvent {
    /// Counts up per viewer, from 1. Echoed back in [`Frame::input_seq`].
    pub seq: u64,
    /// Viewer clock when the input happened, in microseconds.
    pub sent_us: u64,
    pub input: Input,
}

/// The file behind a document surface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub name: String,
    pub mime: String,
    pub data: Bytes,
}

/// One open surface.
#[async_trait]
pub trait Surface: Send + Sync {
    fn info(&self) -> SurfaceInfo;

    /// Pixel mode: subscribes to frames. A slow receiver loses old frames
    /// (`RecvError::Lagged`) and must wait for the next keyframe when the
    /// encoding is a video codec. Document mode: `Mode` error.
    fn frames(&self) -> Result<broadcast::Receiver<Arc<Frame>>>;

    /// Asks for a keyframe on the frame stream, e.g. after a lag.
    async fn request_keyframe(&self) -> Result<()>;

    /// Pixel mode: delivers one input event.
    async fn input(&self, event: InputEvent) -> Result<()>;

    /// Document mode: the file, sent once. Pixel mode: `Mode` error.
    async fn document(&self) -> Result<Document>;

    /// The current picture as PNG, for `surface_screenshot`.
    async fn screenshot(&self) -> Result<Bytes>;
}

/// A source of surfaces: the browser, a document viewer, a VM.
#[async_trait]
pub trait SurfaceServer: Send + Sync {
    /// Server part of its uris, e.g. `browser`.
    fn name(&self) -> &str;

    /// Surfaces that exist right now.
    async fn list(&self) -> Result<Vec<SurfaceInfo>>;

    async fn open(&self, uri: &SurfaceUri) -> Result<Arc<dyn Surface>>;
}
