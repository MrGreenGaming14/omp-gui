//! Turning frames into pixels: still encodings here, video through a port.

use crate::framebuffer::Framebuffer;
use loa_surfaces::pixels::{self, PixelError};
use loa_contracts::surface::{Encoding, Frame};

pub use loa_surfaces::pixels::STILL_ENCODINGS;

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Pixels(#[from] PixelError),
    #[error("video decoder: {0}")]
    Video(String),
    #[error("{0:?} needs a video decoder")]
    NeedsVideo(Encoding),
}

/// Port: decodes H.264 and HEVC access units. Backed by a hardware or
/// software decoder the app links in.
pub trait VideoDecoder: Send {
    /// Whether this decoder handles `encoding`.
    fn supports(&self, encoding: Encoding) -> bool;

    /// Feeds one access unit and writes the resulting picture, full surface
    /// RGBA of `frame.width` by `frame.height`, into `out`. `Ok(false)` when
    /// the unit produced no picture yet.
    fn decode(&mut self, frame: &Frame, out: &mut [u8]) -> Result<bool, DecodeError>;

    /// Forgets reference pictures, e.g. after frames were lost. The next
    /// frame fed is a keyframe.
    fn reset(&mut self);
}

/// Whether frames of `encoding` need a [`VideoDecoder`].
pub fn is_video(encoding: Encoding) -> bool {
    matches!(encoding, Encoding::H264 | Encoding::Hevc)
}

/// Applies an RGBA, PNG or JPEG frame onto `fb` at its region. `fb` is left
/// unchanged on error.
pub fn apply_still(fb: &mut Framebuffer, frame: &Frame) -> Result<(), DecodeError> {
    let (width, height) = (fb.width(), fb.height());
    pixels::apply_still(fb.pixels_mut(), width, height, frame).map_err(|e| match e {
        PixelError::NotStill(video) => DecodeError::NeedsVideo(video),
        other => other.into(),
    })
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
