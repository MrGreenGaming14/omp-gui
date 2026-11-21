//! Decoding H.264 access units into RGBA pictures, in process
//! (`rusty_h264`), one picture per access unit with no delay.
//!
//! HEVC has no in-process decoder: piping it through an `ffmpeg` process
//! shows every picture one access unit late (ffmpeg holds the newest one),
//! which a live surface can't afford. Viewers built on this decoder accept
//! H.264 only; HEVC streams go to viewers that decode it themselves.

use crate::{BenchError, Result, yuv};
use loa_contracts::surface::{Encoding, Frame, Rect};
use rusty_h264::Decoder;

/// Stateful H.264 decoder: references persist between access units of one
/// stream until [`reset`](Self::reset).
pub struct VideoDecoder {
    h264: Decoder,
}

impl Default for VideoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoDecoder {
    pub fn new() -> Self {
        Self { h264: Decoder::new() }
    }

    pub fn supports(&self, encoding: Encoding) -> bool {
        encoding == Encoding::H264
    }

    /// The video encodings this decoder takes, best first.
    pub fn encodings(&self) -> Vec<Encoding> {
        vec![Encoding::H264]
    }

    /// Decodes one access unit into `out`, a tightly packed RGBA picture of
    /// `frame.width` by `frame.height`. `Ok(false)` when the unit carried no
    /// picture.
    pub fn decode(&mut self, frame: &Frame, out: &mut [u8]) -> Result<bool> {
        let full = Rect { x: 0, y: 0, width: frame.width, height: frame.height };
        if frame.region != full {
            return Err(BenchError::Invalid("video frames must cover the whole picture".into()));
        }
        if out.len() != frame.width as usize * frame.height as usize * 4 {
            return Err(BenchError::Invalid(format!(
                "{} bytes for a {}x{} RGBA picture",
                out.len(),
                frame.width,
                frame.height
            )));
        }
        if frame.encoding != Encoding::H264 {
            return Err(BenchError::NoDecoder(frame.encoding));
        }
        let error = |message: String| BenchError::Decode { encoding: Encoding::H264, message };
        let Some(decoded) = self.h264.decode(&frame.data).map_err(|e| error(e.to_string()))? else {
            return Ok(false);
        };
        if decoded.width != frame.width as usize || decoded.height != frame.height as usize {
            let message = format!("picture is {}x{}, the frame {}x{}", decoded.width, decoded.height, frame.width, frame.height);
            self.h264.recycle_frame(decoded);
            return Err(error(message));
        }
        yuv::i420_to_rgba(
            yuv::I420 {
                width: decoded.width,
                height: decoded.height,
                y: &decoded.y,
                u: &decoded.u,
                v: &decoded.v,
                stride_y: decoded.width,
                stride_c: decoded.width / 2,
            },
            out,
        );
        self.h264.recycle_frame(decoded);
        Ok(true)
    }

    /// Forgets reference pictures; the next unit fed must be a keyframe.
    pub fn reset(&mut self) {
        self.h264 = Decoder::new();
    }
}
