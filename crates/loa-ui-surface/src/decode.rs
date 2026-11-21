//! Turning frames into pixels: a chain of the app's [`FrameDecoder`]s with its still decoder
//! last. What the chain accepts is what a tab negotiates with its surface.

use crate::framebuffer::Framebuffer;
use loa_view::surface::port::{DecodeError, Decoders, FrameDecoder};
use loa_view::surface::{Encoding, Frame};

/// The decoders of one tab: the extra one first, then the still one.
pub(crate) struct Chain(Vec<Box<dyn FrameDecoder>>);

impl Chain {
    pub fn new(decoders: &Decoders) -> Self {
        Self(decoders.extra.iter().map(|make| make()).chain([(decoders.still)()]).collect())
    }

    /// What the chain decodes, best first.
    pub fn accepted(&self) -> Vec<Encoding> {
        self.0.iter().flat_map(|d| d.accepts().iter().copied()).collect()
    }

    /// What the still decoder (last in the chain) decodes: asked for when a stream can't be.
    pub fn still(&self) -> Vec<Encoding> {
        self.0.last().map(|d| d.accepts().to_vec()).unwrap_or_default()
    }

    /// The first decoder accepting `encoding`.
    pub fn find(&mut self, encoding: Encoding) -> Option<&mut Box<dyn FrameDecoder>> {
        self.0.iter_mut().find(|d| d.accepts().contains(&encoding))
    }

    pub fn reset(&mut self) {
        self.0.iter_mut().for_each(|d| d.reset());
    }
}

/// Applies `frame` onto `fb` with `decoder`.
pub(crate) fn apply(decoder: &mut dyn FrameDecoder, frame: &Frame, fb: &mut Framebuffer) -> Result<bool, DecodeError> {
    let (width, height) = (fb.width(), fb.height());
    decoder.apply(frame, fb.pixels_mut(), width, height)
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
