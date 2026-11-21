//! Images: PNG, JPEG, GIF (first frame), WebP and BMP, one page at their pixel size.

use crate::docs::{DocsError, Fonts, Format, PageSize, Paged, RenderOpts, Result, Rgba, Sniff};

/// A decoded image.
struct ImageDoc {
    pixels: ::image::RgbaImage,
}

/// Decodes `bytes` as `format`. An animated GIF yields its first frame.
pub(crate) fn open(format: Format, bytes: &[u8]) -> Result<Box<dyn Paged>> {
    let Sniff::Image(image_format) = format.spec().sniff else {
        return Err(DocsError::Unsupported(format!("{format} is not an image format")));
    };
    let image = ::image::load_from_memory_with_format(bytes, image_format).map_err(|e| DocsError::invalid(format, e))?;
    Ok(Box::new(ImageDoc { pixels: image.into_rgba8() }))
}

impl Paged for ImageDoc {
    fn page_count(&self) -> usize {
        1
    }

    fn page_size(&self, index: usize) -> Result<PageSize> {
        let (width, height) = (self.pixels.width() as f32, self.pixels.height() as f32);
        (index == 0).then_some(PageSize { width, height }).ok_or(DocsError::PageRange { index, count: 1 })
    }

    fn render(&self, index: usize, scale: f32, _: &Fonts, _: &RenderOpts) -> Result<Rgba> {
        let (width, height) = self.page_size(index)?.pixels(scale);
        let data = if (width, height) == self.pixels.dimensions() {
            self.pixels.as_raw().clone()
        } else {
            ::image::imageops::resize(&self.pixels, width, height, ::image::imageops::FilterType::Triangle).into_raw()
        };
        Ok(Rgba { width, height, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::{DynamicImage, Rgba as Px, RgbaImage};

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
    }
}
