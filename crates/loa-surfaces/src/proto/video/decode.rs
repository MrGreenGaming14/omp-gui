//! Rebuilding a surface picture from its frames: stills and in-process H.264
//! (one picture per access unit). HEVC is left to viewers that decode it.

use super::video_error;
use crate::pixels::{self, RgbaImage};
use crate::proto::Result;
use loa_contracts::surface::{Encoding, Frame, Rect};

/// Stateful H.264 decoder: references persist until [`reset`](Self::reset).
#[derive(Default)]
pub struct VideoDecoder(rusty_h264::Decoder);

impl VideoDecoder {
    pub fn supports(&self, encoding: Encoding) -> bool {
        encoding == Encoding::H264
    }

    /// Decodes one whole-picture access unit into `out` (tight RGBA of the
    /// frame's size). `Ok(false)` when the unit carried no picture.
    pub fn decode(&mut self, frame: &Frame, out: &mut [u8]) -> Result<bool> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        if frame.region != (Rect { x: 0, y: 0, width: frame.width, height: frame.height }) || out.len() != w * h * 4 {
            return Err(video_error("video frames must cover the whole picture"));
        }
        if frame.encoding != Encoding::H264 {
            return Err(video_error(format!("no decoder for {:?} frames", frame.encoding)));
        }
        let Some(pic) = self.0.decode(&frame.data).map_err(|e| video_error(e.to_string()))? else {
            return Ok(false);
        };
        let size = (pic.width, pic.height);
        if size == (w, h) {
            for (row, dst) in out.chunks_exact_mut(w * 4).enumerate() {
                let (ys, us, vs) = (&pic.y[row * w..], &pic.u[(row / 2) * (w / 2)..], &pic.v[(row / 2) * (w / 2)..]);
                for (x, px) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let c = 298 * (i32::from(ys[x]) - 16);
                    let (d, e) = (i32::from(us[x / 2]) - 128, i32::from(vs[x / 2]) - 128);
                    let clamp = |v: i32| (v >> 8).clamp(0, 255) as u8;
                    *px = [clamp(c + 409 * e + 128), clamp(c - 100 * d - 208 * e + 128), clamp(c + 516 * d + 128), 255];
                }
            }
        }
        self.0.recycle_frame(pic);
        if size != (w, h) {
            return Err(video_error(format!("picture is {}x{}, the frame {w}x{h}", size.0, size.1)));
        }
        Ok(true)
    }

    /// Forgets reference pictures; the next unit must be a keyframe.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Applies frames of any encoding to an [`RgbaImage`].
#[derive(Default)]
pub struct FrameDecoder(VideoDecoder);

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a frame to `canvas`, resizing it to the frame's picture.
    pub fn apply(&mut self, frame: &Frame, canvas: &mut RgbaImage) -> Result<()> {
        let empty = frame.region.width.min(frame.region.height) == 0;
        if empty || frame.width.max(frame.height) > 8192 || !pixels::region_fits(frame.region, frame.width, frame.height) {
            return Err(video_error("frame region is outside its picture"));
        }
        if (frame.width, frame.height) != (canvas.width, canvas.height) {
            *canvas = RgbaImage::new(frame.width, frame.height);
        }
        match frame.encoding {
            Encoding::H264 | Encoding::Hevc if self.0.decode(frame, &mut canvas.data)? => Ok(()),
            Encoding::H264 | Encoding::Hevc => Err(video_error("access unit contains no picture")),
            _ => pixels::apply_still(&mut canvas.data, canvas.width, canvas.height, frame)
                .map_err(|e| video_error(e.to_string())),
        }
    }
}
