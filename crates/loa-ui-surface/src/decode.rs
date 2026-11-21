//! Turning frames into pixels: a chain of [`FrameDecoder`] strategies with [`StillDecoder`]
//! last. What the chain accepts is what a tab negotiates with its surface.

use crate::framebuffer::Framebuffer;
use loa_contracts::surface::{Encoding, Frame};
use loa_surfaces::pixels::{self, PixelError, STILL_ENCODINGS};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Pixels(#[from] PixelError),
    #[error("video decoder: {0}")]
    Video(String),
}

/// One decoding strategy of a pixel tab.
pub trait FrameDecoder: Send {
    /// The encodings this decoder applies, best first.
    fn accepts(&self) -> &[Encoding];

    /// Applies `frame` onto `fb`; `Ok(false)` when it produced no picture yet.
    fn apply(&mut self, frame: &Frame, fb: &mut Framebuffer) -> Result<bool, DecodeError>;

    /// Forgets reference pictures after lost frames; the next frame fed is a keyframe.
    fn reset(&mut self) {}
}

/// RGBA, PNG and JPEG regions, written onto the retained picture.
pub struct StillDecoder;

impl FrameDecoder for StillDecoder {
    fn accepts(&self) -> &[Encoding] {
        &STILL_ENCODINGS
    }

    fn apply(&mut self, frame: &Frame, fb: &mut Framebuffer) -> Result<bool, DecodeError> {
        let (width, height) = (fb.width(), fb.height());
        pixels::apply_still(fb.pixels_mut(), width, height, frame)?;
        Ok(true)
    }
}

/// The decoders of one tab: `extra` first, then [`StillDecoder`].
pub(crate) struct Decoders(Vec<Box<dyn FrameDecoder>>);

impl Decoders {
    pub fn new(extra: Option<Box<dyn FrameDecoder>>) -> Self {
        Self(extra.into_iter().chain([Box::new(StillDecoder) as Box<dyn FrameDecoder>]).collect())
    }

    /// What the chain decodes, best first.
    pub fn accepted(&self) -> Vec<Encoding> {
        self.0.iter().flat_map(|d| d.accepts().iter().copied()).collect()
    }

    /// The first decoder accepting `encoding`.
    pub fn find(&mut self, encoding: Encoding) -> Option<&mut Box<dyn FrameDecoder>> {
        self.0.iter_mut().find(|d| d.accepts().contains(&encoding))
    }

    pub fn reset(&mut self) {
        self.0.iter_mut().for_each(|d| d.reset());
    }
}

/// Whether frames of `encoding` reference earlier ones, so a lost frame needs a keyframe.
pub fn is_video(encoding: Encoding) -> bool {
    matches!(encoding, Encoding::H264 | Encoding::Hevc)
}

/// A short name for messages on the tab.
pub fn encoding_name(encoding: Encoding) -> &'static str {
    match encoding {
        Encoding::Rgba => "RGBA",
        Encoding::Jpeg => "JPEG",
        Encoding::Png => "PNG",
        Encoding::H264 => "H.264",
        Encoding::Hevc => "HEVC",
    }
}
