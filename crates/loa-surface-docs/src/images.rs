//! Images (#129): PNG, JPEG, GIF (first frame), WebP and BMP. The image file
//! itself is the document; it is one page at its pixel size.

use crate::{DocsError, Fonts, Format, PageSize, Paged, Result, Rgba};
use ::image::{DynamicImage, ImageFormat, imageops::FilterType};

/// What the agent gets about an image besides the picture itself.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ImageInfo {
    pub format: Format,
    pub mime: &'static str,
    pub width: u32,
    pub height: u32,
}

/// A decoded image.
#[derive(Clone, Debug)]
pub struct ImageDoc {
    format: Format,
    pixels: ::image::RgbaImage,
}

impl ImageDoc {
    /// Decodes `bytes` as `format`. An animated GIF yields its first frame.
    pub fn parse(format: Format, bytes: &[u8]) -> Result<Self> {
        let image_format = match format {
            Format::Png => ImageFormat::Png,
            Format::Jpeg => ImageFormat::Jpeg,
            Format::Gif => ImageFormat::Gif,
            Format::Webp => ImageFormat::WebP,
            Format::Bmp => ImageFormat::Bmp,
            other => return Err(DocsError::Unsupported(format!("{other} is not an image format"))),
        };
        let image = ::image::load_from_memory_with_format(bytes, image_format)
            .map_err(|e| DocsError::invalid(format, e))?;
        let pixels = match image {
            DynamicImage::ImageRgba8(pixels) => pixels,
            other => other.into_rgba8(),
        };
        Ok(Self { format, pixels })
    }

    pub fn info(&self) -> ImageInfo {
        ImageInfo {
            format: self.format,
            mime: self.format.mime(),
            width: self.pixels.width(),
            height: self.pixels.height(),
        }
    }

    /// The image as PNG. A PNG file is returned unchanged.
    pub fn png(&self, original: &[u8]) -> Result<Vec<u8>> {
        if self.format == Format::Png {
            return Ok(original.to_vec());
        }
        Rgba {
            width: self.pixels.width(),
            height: self.pixels.height(),
            data: self.pixels.as_raw().clone(),
        }
        .to_png()
    }
}

impl Paged for ImageDoc {
    fn page_count(&self) -> usize {
        1
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        if index != 0 {
            return Err(DocsError::page_range(index, 1));
        }
        Ok(PageSize { width: self.pixels.width() as f32, height: self.pixels.height() as f32 })
    }

    fn render_page(&self, index: usize, scale: f32, _fonts: &Fonts) -> Result<Rgba> {
        let (width, height) = self.page_size(index)?.pixels(scale);
        let pixels = if (width, height) == self.pixels.dimensions() {
            self.pixels.as_raw().clone()
        } else {
            ::image::imageops::resize(&self.pixels, width, height, FilterType::Triangle).into_raw()
        };
        Ok(Rgba { width, height, data: pixels })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::{Rgba as Px, RgbaImage};
    use std::io::Cursor;

    /// 4 x 2 image, left half red, right half blue.
    fn fixture() -> RgbaImage {
        RgbaImage::from_fn(4, 2, |x, _| if x < 2 { Px([255, 0, 0, 255]) } else { Px([0, 0, 255, 255]) })
    }

    fn encode(format: ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let image = DynamicImage::ImageRgba8(fixture());
        let image = if format == ImageFormat::Jpeg { DynamicImage::ImageRgb8(image.to_rgb8()) } else { image };
        image.write_to(&mut out, format).unwrap();
        out.into_inner()
    }

    #[test]
    fn every_supported_format_decodes_to_its_pixels() {
        for (format, image_format) in [
            (Format::Png, ImageFormat::Png),
            (Format::Gif, ImageFormat::Gif),
            (Format::Webp, ImageFormat::WebP),
            (Format::Bmp, ImageFormat::Bmp),
        ] {
            let bytes = encode(image_format);
            assert_eq!(Format::sniff(&bytes), Some(format));
            let doc = ImageDoc::parse(format, &bytes).unwrap();
            assert_eq!((doc.info().width, doc.info().height), (4, 2), "{format}");
            let page = doc.render_page(0, 1.0, &Fonts::none()).unwrap();
            assert_eq!(page.pixel(0, 0), Some([255, 0, 0, 255]), "{format}");
            assert_eq!(page.pixel(3, 1), Some([0, 0, 255, 255]), "{format}");
        }
    }

    #[test]
    fn jpeg_decodes_close_to_its_colors() {
        let bytes = encode(ImageFormat::Jpeg);
        let doc = ImageDoc::parse(Format::Jpeg, &bytes).unwrap();
        let page = doc.render_page(0, 1.0, &Fonts::none()).unwrap();
        let [r, _, b, a] = page.pixel(0, 0).unwrap();
        assert!(r > 150 && b < 120 && a == 255);
    }

    #[test]
    fn animated_gif_shows_its_first_frame() {
        use ::image::codecs::gif::GifEncoder;
        use ::image::{Delay, Frame};
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            let red = RgbaImage::from_pixel(3, 3, Px([255, 0, 0, 255]));
            let green = RgbaImage::from_pixel(3, 3, Px([0, 255, 0, 255]));
            let delay = Delay::from_numer_denom_ms(100, 1);
            encoder
                .encode_frames([Frame::from_parts(red, 0, 0, delay), Frame::from_parts(green, 0, 0, delay)])
                .unwrap();
        }
        let doc = ImageDoc::parse(Format::Gif, &bytes).unwrap();
        let page = doc.render_page(0, 1.0, &Fonts::none()).unwrap();
        assert_eq!(page.pixel(1, 1), Some([255, 0, 0, 255]));
    }

    #[test]
    fn render_scales_and_png_round_trips() {
        let bytes = encode(ImageFormat::Bmp);
        let doc = ImageDoc::parse(Format::Bmp, &bytes).unwrap();
        let page = doc.render_page(0, 2.0, &Fonts::none()).unwrap();
        assert_eq!((page.width, page.height), (8, 4));
        assert_eq!(page.pixel(0, 0), Some([255, 0, 0, 255]));
        let png = doc.png(&bytes).unwrap();
        let back = ::image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(back, fixture());
        assert!(matches!(doc.page_size(1), Err(DocsError::PageRange { index: 1, count: 1 })));
    }

    #[test]
    fn corrupt_image_is_reported_as_invalid() {
        let mut bytes = encode(ImageFormat::Png);
        bytes.truncate(20);
        assert!(matches!(ImageDoc::parse(Format::Png, &bytes), Err(DocsError::Invalid { .. })));
    }
}
