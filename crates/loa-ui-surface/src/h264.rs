//! The renderer's H.264 decoder: in process, a picture per access unit.

use crate::decode::{DecodeError, VideoDecoder};
use loa_contracts::surface::{Encoding, Frame};

/// [`VideoDecoder`] for H.264 streams (Constrained Baseline, what the live
/// video encoders send).
#[derive(Default)]
pub struct H264Decoder(loa_surface_bench::video::VideoDecoder);

impl H264Decoder {
    pub fn new() -> Self {
        Self::default()
    }
}

impl VideoDecoder for H264Decoder {
    fn supports(&self, encoding: Encoding) -> bool {
        self.0.supports(encoding)
    }

    fn decode(&mut self, frame: &Frame, out: &mut [u8]) -> Result<bool, DecodeError> {
        self.0.decode(frame, out).map_err(|e| DecodeError::Video(e.to_string()))
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}
