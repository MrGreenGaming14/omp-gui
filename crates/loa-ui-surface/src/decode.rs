//! Turning frames into pixels: still encodings here, video through a port.

use crate::framebuffer::{BlitError, Framebuffer};
use loa_contracts::surface::{Encoding, Frame};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error(transparent)]
    Blit(#[from] BlitError),
    #[error("bad {encoding:?} frame: {message}")]
    Image { encoding: Encoding, message: String },
    #[error("decoded picture is {got_width}x{got_height}, its region is {width}x{height}")]
    Size {
        got_width: u32,
        got_height: u32,
        width: u32,
        height: u32,
    },
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

/// Encodings the renderer decodes on its own.
pub const STILL_ENCODINGS: [Encoding; 3] = [Encoding::Rgba, Encoding::Png, Encoding::Jpeg];

/// Applies an RGBA, PNG or JPEG frame onto `fb` at its region. `fb` is left
/// unchanged on error.
pub fn apply_still(fb: &mut Framebuffer, frame: &Frame) -> Result<(), DecodeError> {
    let (width, height, rgba) = match frame.encoding {
        Encoding::Rgba => return Ok(fb.blit(&frame.data, frame.region)?),
        Encoding::Png => {
            let picture = image::load_from_memory_with_format(&frame.data, image::ImageFormat::Png)
                .map_err(|e| DecodeError::Image { encoding: frame.encoding, message: e.to_string() })?
                .into_rgba8();
            (picture.width(), picture.height(), picture.into_raw())
        }
        Encoding::Jpeg => decode_jpeg(frame)?,
        video @ (Encoding::H264 | Encoding::Hevc) => return Err(DecodeError::NeedsVideo(video)),
    };
    let region = frame.region;
    if (width, height) != (region.width, region.height) {
        return Err(DecodeError::Size {
            got_width: width,
            got_height: height,
            width: region.width,
            height: region.height,
        });
    }
    Ok(fb.blit(&rgba, region)?)
}

fn decode_jpeg(frame: &Frame) -> Result<(u32, u32, Vec<u8>), DecodeError> {
    let error = |message: String| DecodeError::Image { encoding: frame.encoding, message };
    let mut decoder = jpeg_decoder::Decoder::new(frame.data.as_ref());
    let pixels = decoder.decode().map_err(|e| error(e.to_string()))?;
    let info = decoder.info().ok_or_else(|| error("JPEG has no frame header".into()))?;
    let count = usize::from(info.width) * usize::from(info.height);
    let mut rgba = Vec::with_capacity(count * 4);
    match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            for rgb in pixels.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
        jpeg_decoder::PixelFormat::L8 => {
            for &gray in &pixels {
                rgba.extend_from_slice(&[gray, gray, gray, 255]);
            }
        }
        jpeg_decoder::PixelFormat::L16 => {
            for gray in pixels.as_chunks::<2>().0 {
                let gray = (u16::from_ne_bytes(*gray) >> 8) as u8;
                rgba.extend_from_slice(&[gray, gray, gray, 255]);
            }
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            for cmyk in pixels.as_chunks::<4>().0 {
                let channel = |value: u8| ((u16::from(255 - value) * u16::from(255 - cmyk[3])) / 255) as u8;
                rgba.extend_from_slice(&[channel(cmyk[0]), channel(cmyk[1]), channel(cmyk[2]), 255]);
            }
        }
    }
    if rgba.len() != count * 4 {
        return Err(error("JPEG decoder returned a short picture".into()));
    }
    Ok((u32::from(info.width), u32::from(info.height), rgba))
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

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use loa_contracts::surface::Rect;

    #[test]
    fn chrome_jpeg_with_one_scan_per_component_keeps_its_colors() {
        let data = Bytes::from_static(include_bytes!("../tests/fixtures/chrome-noninterleaved-red.jpg"));
        let frame = Frame {
            seq: 1,
            captured_us: 0,
            input_seq: 0,
            width: 320,
            height: 240,
            region: Rect { x: 0, y: 0, width: 320, height: 240 },
            encoding: Encoding::Jpeg,
            keyframe: true,
            data,
        };
        let mut fb = Framebuffer::new(320, 240).unwrap();
        apply_still(&mut fb, &frame).unwrap();
        let center = ((120 * 320 + 160) * 4) as usize;
        let [r, g, b, a] = fb.pixels()[center..center + 4] else { unreachable!() };
        assert!(r > 240 && g < 15 && b < 15 && a == 255, "decoded {:?}", [r, g, b, a]);
    }
}
