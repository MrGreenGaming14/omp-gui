//! The Surfaces panel: browser tabs, documents, VMs and declared surfaces, drawn by the
//! surface renderer. The protocol values and the live [`Surface`] transport are the
//! foundation's; the launcher's status types and [`Intent`] are this screen's own, and
//! [`port`] holds what the app implements for decoding and document rendering.

use std::path::PathBuf;

pub use loa_contracts::surface::{
    Document, Encoding, Frame, Input, InputEvent, Modifiers, MouseButton, Rect, StreamInfo, Surface, SurfaceError, SurfaceInfo,
    SurfaceMode, SurfaceUri,
};

/// Server name of the built-in document surfaces.
pub const DOCUMENT_SERVER: &str = "docs";

/// Whether browser surfaces can be opened, as the launcher says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserStatus {
    /// The browser executable, and whether Chrome's sandbox is on.
    Ready { executable: PathBuf, sandboxed: bool },
    Missing,
}

/// How browser frames are captured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Capture {
    /// Headless Chrome's JPEG screencast.
    #[default]
    Screencast,
    /// Headed Chrome on an Xvfb virtual display per tab, captured raw.
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

/// A surface that can be opened: a configured VM endpoint, or one an MCP server declares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableSurface {
    pub info: SurfaceInfo,
    /// The server declaring it, e.g. `vm` or the MCP server's name.
    pub server: String,
}

/// Live video encoding for viewers across the network, as the launcher shows it. Tabs in this
/// app always get the source frames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoStatus {
    pub enabled: bool,
    /// What network viewers get; `None` while the machine's encoders are tried.
    pub encoder: Option<StreamInfo>,
    /// Hardware encoders that work here, e.g. `h264_vaapi`.
    pub hardware: Vec<String>,
}

/// What the Surfaces panel asks of the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    OpenBrowser(String),
    OpenDocument(PathBuf),
    OpenVm { address: String, password: Option<String> },
    /// Open a listed surface.
    Open(SurfaceUri),
    /// The user closed the tab of an open surface.
    Close(SurfaceUri),
    /// An open surface streams `encoding`, which the renderer can't decode; the renderer has
    /// asked the surface to switch to one of `supported`.
    EncodingUnsupported { uri: SurfaceUri, encoding: Encoding, supported: Vec<Encoding> },
    /// The user turned live video encoding on or off (only with live video built in).
    SetVideo(bool),
    /// The user picked how browser frames are captured.
    SetCapture(Capture),
}

/// What the surface renderer needs from the app: frame decoders and the document renderer.
pub mod port {
    use super::{Document, Encoding, Frame};
    use std::sync::Arc;

    /// Why a frame couldn't be decoded, as the tab shows it.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct DecodeError(pub String);

    impl std::fmt::Display for DecodeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for DecodeError {}

    /// One decoding strategy of a pixel tab.
    pub trait FrameDecoder: Send {
        /// The encodings this decoder applies, best first.
        fn accepts(&self) -> &[Encoding];

        /// Applies `frame` onto `picture`, the `width` x `height` RGBA picture the tab
        /// retains; `Ok(false)` when it produced no picture yet.
        fn apply(&mut self, frame: &Frame, picture: &mut [u8], width: u32, height: u32) -> Result<bool, DecodeError>;

        /// Forgets reference pictures after lost frames; the next frame fed is a keyframe.
        fn reset(&mut self) {}
    }

    /// Makes one decoder for a new pixel tab.
    pub type NewDecoder = Arc<dyn Fn() -> Box<dyn FrameDecoder> + Send + Sync>;

    /// The decoders each new pixel tab gets: `extra` first, then `still`.
    #[derive(Clone)]
    pub struct Decoders {
        /// e.g. H.264; without one, tabs ask video surfaces for stills.
        pub extra: Option<NewDecoder>,
        /// The still-image fallback (RGBA, PNG, JPEG); what it accepts is what a tab asks
        /// for when a surface streams something it can't decode.
        pub still: NewDecoder,
    }

    /// Size of a page at zoom 1, in panel pixels.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct PageSize {
        pub width: f32,
        pub height: f32,
    }

    /// A rendered page: 8 bit RGBA, row major, no padding.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct RgbaPage {
        pub width: u32,
        pub height: u32,
        pub data: Vec<u8>,
    }

    /// Straight RGBA colours of rendered pages.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct DocumentPalette {
        pub page: [u8; 4],
        pub text: [u8; 4],
        /// Table borders, header rules and picture placeholders.
        pub rule: [u8; 4],
        /// Sheet grid lines and slide table outlines.
        pub grid: [u8; 4],
        pub header: [u8; 4],
        pub header_text: [u8; 4],
    }

    /// A parsed paged document (PDF, sheet, DOCX, slides).
    pub trait PagedDocument: Send + Sync {
        fn page_count(&self) -> usize;

        /// Size of page `index` (from 0) at zoom 1.
        fn page_size(&self, index: usize) -> Result<PageSize, String>;

        /// Renders page `index` at `zoom` in `palette`.
        fn render(&self, index: usize, zoom: f32, palette: &DocumentPalette) -> Result<RgbaPage, String>;
    }

    /// A fetched document, ready to show.
    pub enum OpenedDocument {
        /// An image the renderer shows from the document's bytes as they are.
        Image { width: u32, height: u32 },
        Paged(Box<dyn PagedDocument>),
    }

    /// Parses fetched documents for local rendering.
    pub trait DocumentRenderer: Send + Sync {
        /// File extensions the picker lists, compared case insensitively.
        fn extensions(&self) -> Vec<String>;

        /// Opens `document`, or says why it can't be shown.
        fn open(&self, document: &Document) -> Result<OpenedDocument, String>;
    }
}
