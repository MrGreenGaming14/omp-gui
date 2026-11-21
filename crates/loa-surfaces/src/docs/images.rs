//! Images: PNG, JPEG, GIF (first frame), WebP and BMP, one page at their pixel size.

use crate::docs::{DocsError, Fonts, Format, PageSize, Paged, RenderOpts, Result, Rgba, Sniff};
use ::image::{DynamicImage, imageops::FilterType};

/// A decoded image.
#[derive(Clone, Debug)]
pub struct ImageDoc {
    pixels: ::image::RgbaImage,
}

pub(crate) fn open(format: Format, bytes: &[u8]) -> Result<Box<dyn Paged>> {
    Ok(Box::new(ImageDoc::parse(format, bytes)?))
}

impl ImageDoc {
    /// Decodes `bytes` as `format`. An animated GIF yields its first frame.
    pub fn parse(format: Format, bytes: &[u8]) -> Result<Self> {
        let Sniff::Image(image_format) = format.spec().sniff else {
            return Err(DocsError::Unsupported(format!("{format} is not an image format")));
        };
        let image = ::image::load_from_memory_with_format(bytes, image_format).map_err(|e| DocsError::invalid(format, e))?;
        let pixels = match image {
            DynamicImage::ImageRgba8(pixels) => pixels,
            other => other.into_rgba8(),
        };
        Ok(Self { pixels })
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

    fn render(&self, index: usize, scale: f32, _: &Fonts, _: &RenderOpts) -> Result<Rgba> {
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
    use ::image::codecs::gif::GifEncoder;
    use ::image::{Delay, Frame, Rgba as Px, RgbaImage};

    #[test]
    fn every_image_format_decodes_scales_and_reports_corruption() {
        // 4 x 2, left half red, right half blue.
        let fixture = RgbaImage::from_fn(4, 2, |x, _| Px(if x < 2 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }));
        for format in [Format::Png, Format::Gif, Format::Webp, Format::Bmp, Format::Jpeg] {
            let Sniff::Image(image_format) = format.spec().sniff else { unreachable!() };
            let mut bytes = std::io::Cursor::new(Vec::new());
            let image = DynamicImage::ImageRgba8(fixture.clone());
            let image = if format == Format::Jpeg { DynamicImage::ImageRgb8(image.to_rgb8()) } else { image };
            image.write_to(&mut bytes, image_format).unwrap();
            let bytes = bytes.into_inner();
            assert_eq!(Format::sniff(&bytes), Some(format));
            let page = open(format, &bytes).unwrap().render_page(0, 2.0, &Fonts::none()).unwrap();
            assert_eq!((page.width, page.height), (8, 4));
            let [r, _, b, a] = page.pixel(0, 0).unwrap();
            assert!(r > 150 && b < 120 && a == 255, "{format}");
            assert!(matches!(open(format, &bytes[..20]), Err(DocsError::Invalid { .. })), "{format}");
        }
        let mut gif = Vec::new();
        let frames = [[255, 0, 0, 255], [0, 255, 0, 255]].map(|c| Frame::from_parts(RgbaImage::from_pixel(3, 3, Px(c)), 0, 0, Delay::from_numer_denom_ms(100, 1)));
        GifEncoder::new(&mut gif).encode_frames(frames).unwrap();
        let first = ImageDoc::parse(Format::Gif, &gif).unwrap().render_page(0, 1.0, &Fonts::none()).unwrap();
        assert_eq!(first.pixel(1, 1), Some([255, 0, 0, 255]), "an animated GIF shows its first frame");
        assert!(matches!(ImageDoc::parse(Format::Pdf, &gif), Err(DocsError::Unsupported(_))));
    }
}
