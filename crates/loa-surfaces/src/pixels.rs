//! RGBA pictures and the still codecs every surface and viewer shares.
//!
//! Pixels are 8 bit RGBA, straight alpha, row major, no padding: the layout
//! of [`Encoding::Rgba`].

use bytes::Bytes;
use loa_contracts::surface::{Encoding, Frame, Rect};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PixelError {
    #[error("region {region:?} is outside the {width}x{height} picture")]
    OutOfBounds { region: Rect, width: u32, height: u32 },
    #[error("region needs {expected} bytes of RGBA, got {actual}")]
    Length { expected: usize, actual: usize },
    #[error("a {width}x{height} picture needs {expected} bytes, it has {actual}")]
    Target { width: u32, height: u32, expected: usize, actual: usize },
    #[error("decoded picture is {got_width}x{got_height}, its region is {width}x{height}")]
    Size { got_width: u32, got_height: u32, width: u32, height: u32 },
    #[error("{0:?} is not a still encoding")]
    NotStill(Encoding),
    #[error("{format}: {message}")]
    Codec { format: &'static str, message: String },
}

fn codec(format: &'static str) -> impl Fn(String) -> PixelError {
    move |message| PixelError::Codec { format, message }
}

/// Bytes of a `width` by `height` RGBA picture, `None` on overflow.
pub fn rgba_len(width: u32, height: u32) -> Option<usize> {
    (width as usize).checked_mul(height as usize)?.checked_mul(4)
}

/// Whether `region` lies inside a `width` by `height` picture.
pub fn region_fits(region: Rect, width: u32, height: u32) -> bool {
    u64::from(region.x) + u64::from(region.width) <= u64::from(width)
        && u64::from(region.y) + u64::from(region.height) <= u64::from(height)
}

/// Copies the RGBA rows of `src` into `region` of `dst`, a `width` by
/// `height` picture. Nothing is written when the sizes don't match.
pub fn blit(dst: &mut [u8], width: u32, height: u32, src: &[u8], region: Rect) -> Result<(), PixelError> {
    let expected = rgba_len(width, height).unwrap_or(usize::MAX);
    if dst.len() != expected {
        return Err(PixelError::Target { width, height, expected, actual: dst.len() });
    }
    if !region_fits(region, width, height) {
        return Err(PixelError::OutOfBounds { region, width, height });
    }
    // The region fits inside a picture whose byte length fits in usize.
    let row = region.width as usize * 4;
    if src.len() != row * region.height as usize {
        return Err(PixelError::Length { expected: row * region.height as usize, actual: src.len() });
    }
    if row == 0 {
        return Ok(());
    }
    let stride = width as usize * 4;
    for (i, line) in src.chunks_exact(row).enumerate() {
        let start = (region.y as usize + i) * stride + region.x as usize * 4;
        dst[start..start + row].copy_from_slice(line);
    }
    Ok(())
}

/// The first and last row that differ between two pictures of `width`, or
/// `None` when they are equal.
pub fn changed_rows(previous: &[u8], current: &[u8], width: u32) -> Option<(u32, u32)> {
    let row = (width as usize * 4).max(1);
    let rows = current.len() / row;
    let changed = |r: &usize| current.get(r * row..(r + 1) * row) != previous.get(r * row..(r + 1) * row);
    let first = (0..rows).find(changed)?;
    let last = (first..rows).rev().find(changed).unwrap_or(first);
    Some((first as u32, last as u32))
}

/// An owned RGBA picture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl RgbaImage {
    /// A picture of one colour.
    pub fn filled(width: u32, height: u32, pixel: [u8; 4]) -> Self {
        Self { width, height, data: pixel.repeat(width as usize * height as usize) }
    }

    /// Transparent black.
    pub fn new(width: u32, height: u32) -> Self {
        Self::filled(width, height, [0; 4])
    }

    pub fn full(&self) -> Rect {
        Rect { x: 0, y: 0, width: self.width, height: self.height }
    }

    /// The pixel at `(x, y)`.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        (x < self.width && y < self.height).then(|| {
            let i = (y as usize * self.width as usize + x as usize) * 4;
            [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
        })
    }

    /// Writes `src`, RGBA rows covering `region`.
    pub fn blit(&mut self, region: Rect, src: &[u8]) -> Result<(), PixelError> {
        blit(&mut self.data, self.width, self.height, src, region)
    }

    /// The pixels of `region`, which lies inside the picture.
    pub fn crop(&self, region: Rect) -> Bytes {
        if region == self.full() {
            return Bytes::copy_from_slice(&self.data);
        }
        let (row, stride) = (region.width as usize * 4, self.width as usize * 4);
        let mut out = Vec::with_capacity(row * region.height as usize);
        for y in region.y as usize..(region.y + region.height) as usize {
            let start = y * stride + region.x as usize * 4;
            out.extend_from_slice(&self.data[start..start + row]);
        }
        out.into()
    }

    /// Paints `region`, clipped to the picture, with `pixel`.
    pub fn fill(&mut self, region: Rect, pixel: [u8; 4]) {
        let x1 = (region.x.saturating_add(region.width)).min(self.width) as usize;
        let y1 = (region.y.saturating_add(region.height)).min(self.height) as usize;
        for y in region.y as usize..y1 {
            let row = y * self.width as usize;
            for x in region.x as usize..x1 {
                self.data[(row + x) * 4..(row + x + 1) * 4].copy_from_slice(&pixel);
            }
        }
    }

    /// Copies the `region` sized area at `(src_x, src_y)` to `region`. Both
    /// lie inside the picture and may overlap.
    pub fn copy_rect(&mut self, src_x: u32, src_y: u32, region: Rect) {
        let (row, stride) = (region.width as usize * 4, self.width as usize * 4);
        let mut copy = |i: usize| {
            let from = (src_y as usize + i) * stride + src_x as usize * 4;
            let to = (region.y as usize + i) * stride + region.x as usize * 4;
            self.data.copy_within(from..from + row, to);
        };
        if region.y > src_y {
            (0..region.height as usize).rev().for_each(&mut copy);
        } else {
            (0..region.height as usize).for_each(copy);
        }
    }

    pub fn to_png(&self) -> Result<Vec<u8>, PixelError> {
        encode_png(self.width, self.height, &self.data)
    }
}

/// Encodes RGBA pixels as PNG.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, PixelError> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| codec("PNG")(e.to_string()))?;
    Ok(out)
}

pub fn decode_png(data: &[u8]) -> Result<RgbaImage, PixelError> {
    let image = image::load_from_memory_with_format(data, image::ImageFormat::Png)
        .map_err(|e| codec("PNG")(e.to_string()))?
        .into_rgba8();
    Ok(RgbaImage { width: image.width(), height: image.height(), data: image.into_raw() })
}

/// Decodes a JPEG with `jpeg-decoder`: image 0.25's own JPEG backend
/// mis-decodes baseline JPEGs with one scan per component, which is how
/// Chrome encodes some screencast frames.
pub fn decode_jpeg(data: &[u8]) -> Result<RgbaImage, PixelError> {
    let error = codec("JPEG");
    let mut decoder = jpeg_decoder::Decoder::new(data);
    let pixels = decoder.decode().map_err(|e| error(e.to_string()))?;
    let info = decoder.info().ok_or_else(|| error("no frame header".into()))?;
    let rgba: Vec<u8> = match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        jpeg_decoder::PixelFormat::L8 => pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        jpeg_decoder::PixelFormat::L16 => {
            pixels.as_chunks::<2>().0.iter().map(|g| (u16::from_ne_bytes(*g) >> 8) as u8).flat_map(|g| [g, g, g, 255]).collect()
        }
        jpeg_decoder::PixelFormat::CMYK32 => pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| {
                let channel = |v: u8| ((u16::from(255 - v) * u16::from(255 - p[3])) / 255) as u8;
                [channel(p[0]), channel(p[1]), channel(p[2]), 255]
            })
            .collect(),
    };
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    if Some(rgba.len()) != rgba_len(width, height) {
        return Err(error("short picture".into()));
    }
    Ok(RgbaImage { width, height, data: rgba })
}

/// Encodings [`apply_still`] decodes.
pub const STILL_ENCODINGS: [Encoding; 3] = [Encoding::Rgba, Encoding::Png, Encoding::Jpeg];

/// Applies an RGBA, PNG or JPEG frame at its region onto `dst`, a `width`
/// by `height` picture. `dst` is left unchanged on error.
pub fn apply_still(dst: &mut [u8], width: u32, height: u32, frame: &Frame) -> Result<(), PixelError> {
    let region = frame.region;
    let picture = match frame.encoding {
        Encoding::Rgba => return blit(dst, width, height, &frame.data, region),
        Encoding::Png => decode_png(&frame.data)?,
        Encoding::Jpeg => decode_jpeg(&frame.data)?,
        video => return Err(PixelError::NotStill(video)),
    };
    if (picture.width, picture.height) != (region.width, region.height) {
        let (got_width, got_height) = (picture.width, picture.height);
        return Err(PixelError::Size { got_width, got_height, width: region.width, height: region.height });
    }
    blit(dst, width, height, &picture.data, region)
}

/// Width and height from a PNG header.
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || &png[..8] != b"\x89PNG\r\n\x1a\n" || &png[12..16] != b"IHDR" {
        return None;
    }
    let size = |at: usize| u32::from_be_bytes([png[at], png[at + 1], png[at + 2], png[at + 3]]);
    Some((size(16), size(20)))
}

/// Width and height of any image format the build decodes, from its header.
pub fn image_size(data: &[u8]) -> Result<(u32, u32), PixelError> {
    let error = codec("image");
    image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| error(e.to_string()))?
        .into_dimensions()
        .map_err(|e| error(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trips_and_reports_its_size() {
        let img = RgbaImage::filled(3, 2, [10, 20, 30, 255]);
        let png = img.to_png().unwrap();
        assert_eq!(png_size(&png), Some((3, 2)));
        assert_eq!(image_size(&png), Ok((3, 2)));
        assert_eq!(decode_png(&png).unwrap(), img);
        assert_eq!(png_size(b"GIF89a"), None);
    }

    #[test]
    fn chrome_jpeg_with_one_scan_per_component_keeps_its_colors() {
        let jpeg = include_bytes!("../tests/fixtures/chrome-noninterleaved-red.jpg");
        let [r, g, b, a] = decode_jpeg(jpeg).unwrap().pixel(160, 120).unwrap();
        assert!(r > 240 && g < 15 && b < 15 && a == 255, "decoded {:?}", [r, g, b, a]);
    }
}
