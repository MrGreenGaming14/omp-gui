//! Rebuilding a surface picture from its frames: stills (RGBA, JPEG, PNG)
//! and in-process H.264 (`rusty_h264`, one picture per access unit). HEVC
//! has no in-process decoder; it goes to viewers that decode it themselves.

use super::video_error;
use crate::Result;
use loa_contracts::surface::{Encoding, Frame, Rect};

/// An RGBA picture, row major, no padding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Canvas {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, rgba: vec![0; width as usize * height as usize * 4] }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Copies tightly packed RGBA `pixels` covering `region` into the canvas.
    pub fn blit(&mut self, region: Rect, pixels: &[u8]) -> Result<()> {
        let row_len = region.width as usize * 4;
        let inside = u64::from(region.x) + u64::from(region.width) <= u64::from(self.width)
            && u64::from(region.y) + u64::from(region.height) <= u64::from(self.height);
        if !inside || pixels.len() != row_len * region.height as usize {
            return Err(video_error(format!("{} bytes for {region:?} of a {}x{} canvas", pixels.len(), self.width, self.height)));
        }
        let stride = self.width as usize * 4;
        for (row, src) in pixels.chunks_exact(row_len.max(1)).enumerate() {
            let start = (region.y as usize + row) * stride + region.x as usize * 4;
            self.rgba[start..start + row_len].copy_from_slice(src);
        }
        Ok(())
    }
}

/// Stateful H.264 decoder: references persist until [`reset`](Self::reset).
pub struct VideoDecoder(rusty_h264::Decoder);

impl Default for VideoDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoDecoder {
    pub fn new() -> Self {
        Self(rusty_h264::Decoder::new())
    }

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
        let fits = pic.width == w && pic.height == h;
        if fits {
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
        let size = (pic.width, pic.height);
        self.0.recycle_frame(pic);
        if !fits {
            return Err(video_error(format!("picture is {}x{}, the frame {w}x{h}", size.0, size.1)));
        }
        Ok(true)
    }

    /// Forgets reference pictures; the next unit must be a keyframe.
    pub fn reset(&mut self) {
        self.0 = rusty_h264::Decoder::new();
    }
}

/// Applies frames of any encoding to a [`Canvas`].
#[derive(Default)]
pub struct FrameDecoder {
    video: VideoDecoder,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a frame to `canvas`, following the frame's picture size.
    pub fn apply(&mut self, frame: &Frame, canvas: &mut Canvas) -> Result<()> {
        let region = frame.region;
        if region.width == 0
            || region.height == 0
            || u64::from(region.x) + u64::from(region.width) > u64::from(frame.width)
            || u64::from(region.y) + u64::from(region.height) > u64::from(frame.height)
            || frame.width > 8192
            || frame.height > 8192
        {
            return Err(video_error("frame region is outside its picture"));
        }
        if (frame.width, frame.height) != (canvas.width, canvas.height) {
            *canvas = Canvas::new(frame.width, frame.height);
        }
        let pixels = match frame.encoding {
            Encoding::Rgba => return canvas.blit(region, &frame.data),
            Encoding::H264 | Encoding::Hevc => {
                return match self.video.decode(frame, &mut canvas.rgba)? {
                    true => Ok(()),
                    false => Err(video_error("access unit contains no picture")),
                };
            }
            // image's JPEG backend mis-decodes Chrome's one-scan-per-component
            // screencast JPEGs; jpeg-decoder doesn't.
            Encoding::Jpeg => {
                let mut decoder = jpeg_decoder::Decoder::new(frame.data.as_ref());
                let pixels = decoder.decode().map_err(|e| video_error(format!("JPEG: {e}")))?;
                match decoder.info().map(|i| i.pixel_format) {
                    Some(jpeg_decoder::PixelFormat::RGB24) => pixels.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
                    Some(jpeg_decoder::PixelFormat::L8) => pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
                    other => return Err(video_error(format!("unsupported JPEG pixel format {other:?}"))),
                }
            }
            Encoding::Png => {
                let image = image::load_from_memory_with_format(&frame.data, image::ImageFormat::Png)
                    .map_err(|e| video_error(format!("PNG: {e}")))?;
                image.into_rgba8().into_raw()
            }
        };
        canvas.blit(region, &pixels)
    }
}
