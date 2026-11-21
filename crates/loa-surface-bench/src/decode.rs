//! Reconstructs the viewer picture from full and partial surface frames.

use crate::{BenchError, Result, canvas::Canvas, video::VideoDecoder};
use loa_contracts::surface::{Encoding, Frame, Rect};

/// Stateful decoder: video references persist between successive access units.
pub struct FrameDecoder {
    video: VideoDecoder,
    scratch: Vec<u8>,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    /// Stills and H.264.
    pub fn new() -> Self {
        Self { video: VideoDecoder::new(), scratch: Vec::new() }
    }

    /// Applies a frame to the displayed canvas; rejects invalid regions or payloads.
    pub fn apply(&mut self, frame: &Frame, canvas: &mut Canvas) -> Result<()> {
        let region = frame.region;
        if region.width == 0 || region.height == 0 ||
            u64::from(region.x) + u64::from(region.width) > u64::from(frame.width) ||
            u64::from(region.y) + u64::from(region.height) > u64::from(frame.height) {
            return Err(BenchError::Invalid("frame region is outside its picture".into()));
        }
        if frame.width > 8192 || frame.height > 8192 {
            return Err(BenchError::Size { width: frame.width, height: frame.height, reason: "larger than 8192 pixels on a side" });
        }
        let full = Rect { x: 0, y: 0, width: frame.width, height: frame.height };
        if matches!(frame.encoding, Encoding::H264 | Encoding::Hevc) && region != full {
            return Err(BenchError::Invalid("video frames must cover the whole picture".into()));
        }
        canvas.ensure_size(frame.width, frame.height);
        match frame.encoding {
            Encoding::Rgba => canvas.blit(region, &frame.data),
            // zune-jpeg mis-decodes baseline JPEGs with one scan per
            // component, which is how Chrome sends some screencast frames.
            Encoding::Jpeg => {
                let mut decoder = jpeg_decoder::Decoder::new(frame.data.as_ref());
                let pixels = decoder.decode().map_err(|e| decode_error(frame.encoding, e))?;
                let info = decoder.info().ok_or_else(|| decode_error(frame.encoding, "missing JPEG headers"))?;
                if u32::from(info.width) != region.width || u32::from(info.height) != region.height {
                    return Err(decode_error(frame.encoding, "JPEG dimensions do not match region"));
                }
                self.scratch.clear();
                match info.pixel_format {
                    jpeg_decoder::PixelFormat::RGB24 => {
                        for rgb in pixels.as_chunks::<3>().0 {
                            self.scratch.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
                        }
                    }
                    jpeg_decoder::PixelFormat::L8 => {
                        for &gray in &pixels {
                            self.scratch.extend_from_slice(&[gray, gray, gray, 255]);
                        }
                    }
                    other => return Err(decode_error(frame.encoding, format!("unsupported JPEG pixel format {other:?}"))),
                }
                canvas.blit(region, &self.scratch)
            }
            Encoding::Png => {
                let mut decoder = png::Decoder::new(std::io::Cursor::new(frame.data.as_ref()));
                decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
                let mut reader = decoder.read_info().map_err(|e| decode_error(frame.encoding, e))?;
                let size = reader.output_buffer_size().ok_or_else(|| decode_error(frame.encoding, "PNG dimensions overflow"))?;
                self.scratch.resize(size, 0);
                let info = reader.next_frame(&mut self.scratch).map_err(|e| decode_error(frame.encoding, e))?;
                if info.width != region.width || info.height != region.height {
                    return Err(decode_error(frame.encoding, "PNG dimensions do not match region"));
                }
                let n = region.width as usize * region.height as usize;
                let bytes = &self.scratch[..info.buffer_size()];
                match info.color_type {
                    png::ColorType::Rgba => canvas.blit(region, bytes),
                    png::ColorType::Rgb => {
                        let mut rgba = vec![255; n * 4];
                        for (dst, src) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(bytes.as_chunks::<3>().0) {
                            dst[..3].copy_from_slice(src);
                        }
                        canvas.blit(region, &rgba)
                    }
                    png::ColorType::Grayscale => {
                        let mut rgba = vec![255; n * 4];
                        for (dst, &gray) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(bytes) {
                            dst[..3].fill(gray);
                        }
                        canvas.blit(region, &rgba)
                    }
                    png::ColorType::GrayscaleAlpha => {
                        let mut rgba = vec![0; n * 4];
                        for (dst, src) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(bytes.as_chunks::<2>().0) {
                            dst[..3].fill(src[0]);
                            dst[3] = src[1];
                        }
                        canvas.blit(region, &rgba)
                    }
                    png::ColorType::Indexed => Err(decode_error(frame.encoding, "indexed PNG was not expanded")),
                }
            }
            Encoding::H264 | Encoding::Hevc => {
                if self.video.decode(frame, canvas.rgba_mut())? {
                    Ok(())
                } else {
                    Err(decode_error(frame.encoding, "access unit contains no picture"))
                }
            }
        }
    }
}

fn decode_error(encoding: Encoding, error: impl std::fmt::Display) -> BenchError {
    BenchError::Decode { encoding, message: error.to_string() }
}
