//! The surface renderer's ports over `loa-surfaces`: still (and with live video, H.264)
//! frame decoding, and documents parsed and rasterized by the docs page renderers.

use std::sync::Arc;

use loa_surfaces::docs::raster::Color;
use loa_surfaces::docs::{DocPalette, FORMATS, Fonts, Format, Paged, RenderOpts, open_pages};
use loa_surfaces::pixels::{self, STILL_ENCODINGS};
use loa_view::surface::port::{
    DecodeError, Decoders, DocumentPalette, DocumentRenderer, FrameDecoder, OpenedDocument, PageSize, PagedDocument, RgbaPage,
};
use loa_view::surface::{Document, Encoding, Frame};

/// RGBA, PNG and JPEG regions, written onto the retained picture.
pub struct StillDecoder;

impl FrameDecoder for StillDecoder {
    fn accepts(&self) -> &[Encoding] {
        &STILL_ENCODINGS
    }

    fn apply(&mut self, frame: &Frame, picture: &mut [u8], width: u32, height: u32) -> Result<bool, DecodeError> {
        pixels::apply_still(picture, width, height, frame).map_err(|e| DecodeError(e.to_string()))?;
        Ok(true)
    }
}

/// Decodes H.264 streams (Constrained Baseline, what the live video encoders send).
#[cfg(feature = "remote-video")]
#[derive(Default)]
pub struct H264Decoder(loa_surfaces::proto::video::decode::VideoDecoder);

#[cfg(feature = "remote-video")]
impl FrameDecoder for H264Decoder {
    fn accepts(&self) -> &[Encoding] {
        &[Encoding::H264]
    }

    fn apply(&mut self, frame: &Frame, picture: &mut [u8], _: u32, _: u32) -> Result<bool, DecodeError> {
        self.0.decode(frame, picture).map_err(|e| DecodeError(format!("video decoder: {e}")))
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}

/// The decoders of the app's pixel tabs: H.264 first when live video is built in, then stills.
pub fn decoders() -> Decoders {
    #[cfg(feature = "remote-video")]
    let extra: Option<loa_view::surface::port::NewDecoder> = Some(Arc::new(|| Box::new(H264Decoder::default())));
    #[cfg(not(feature = "remote-video"))]
    let extra = None;
    Decoders { extra, still: Arc::new(|| Box::new(StillDecoder)) }
}

/// Opens documents with the docs page renderers, drawing text in `fonts`.
pub struct DocsRenderer {
    fonts: Fonts,
}

impl DocsRenderer {
    pub fn new(fonts: Fonts) -> Self {
        Self { fonts }
    }
}

impl DocumentRenderer for DocsRenderer {
    fn extensions(&self) -> Vec<String> {
        FORMATS.iter().flat_map(|format| format.exts.iter().map(|ext| (*ext).to_owned())).collect()
    }

    fn open(&self, document: &Document) -> Result<OpenedDocument, String> {
        if document.mime.starts_with("image/") {
            let (width, height) = pixels::image_size(&document.data).map_err(|e| e.to_string())?;
            return Ok(OpenedDocument::Image { width, height });
        }
        let format = Format::from_mime(&document.mime).or_else(|| Format::sniff(&document.data));
        let format = format.ok_or_else(|| format!("no viewer for {}", document.mime))?;
        let paged = open_pages(format, &document.data).map_err(|e| e.to_string())?;
        Ok(OpenedDocument::Paged(Box::new(DocsPages { paged, fonts: self.fonts.clone() })))
    }
}

/// A parsed document and the fonts its pages are drawn with.
struct DocsPages {
    paged: Box<dyn Paged>,
    fonts: Fonts,
}

impl PagedDocument for DocsPages {
    fn page_count(&self) -> usize {
        self.paged.page_count()
    }

    fn page_size(&self, index: usize) -> Result<PageSize, String> {
        self.paged.page_size(index).map(|size| PageSize { width: size.width, height: size.height }).map_err(|e| e.to_string())
    }

    fn render(&self, index: usize, zoom: f32, palette: &DocumentPalette) -> Result<RgbaPage, String> {
        let opts = RenderOpts { palette: doc_palette(palette), ..RenderOpts::default() };
        let page = self.paged.render(index, zoom, &self.fonts, &opts).map_err(|e| e.to_string())?;
        Ok(RgbaPage { width: page.width, height: page.height, data: page.data })
    }
}

fn doc_palette(p: &DocumentPalette) -> DocPalette {
    DocPalette {
        page: Color(p.page),
        text: Color(p.text),
        rule: Color(p.rule),
        grid: Color(p.grid),
        header: Color(p.header),
        header_text: Color(p.header_text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use loa_surfaces::pixels::RgbaImage;
    use loa_view::surface::Rect;

    fn frame(seq: u64, region: Rect, encoding: Encoding, data: Vec<u8>) -> Frame {
        let keyframe = region == Rect { x: 0, y: 0, width: 8, height: 8 };
        Frame { seq, captured_us: 0, input_seq: 0, width: 8, height: 8, region, encoding, keyframe, data: Bytes::from(data) }
    }

    fn pixel_at(picture: &[u8], x: usize, y: usize) -> [u8; 4] {
        let i = (y * 8 + x) * 4;
        picture[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn still_frames_land_on_the_retained_picture() {
        let mut jpeg = Vec::new();
        let red = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 4, image::Rgb([200, 0, 0])));
        red.write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
        let rect = |x, y, w, h| Rect { x, y, width: w, height: h };
        // (region, encoding, data): later regions overwrite.
        let frames = [
            (rect(0, 0, 8, 8), Encoding::Rgba, RgbaImage::filled(8, 8, [1, 1, 1, 255]).data),
            (rect(2, 1, 2, 2), Encoding::Rgba, RgbaImage::filled(2, 2, [9, 9, 9, 255]).data),
            (rect(0, 4, 4, 4), Encoding::Png, RgbaImage::filled(4, 4, [0, 200, 0, 255]).to_png().unwrap()),
            (rect(4, 4, 4, 4), Encoding::Jpeg, jpeg),
        ];
        let mut picture = vec![0; 8 * 8 * 4];
        let mut still = StillDecoder;
        for (seq, (region, encoding, data)) in (1..).zip(frames) {
            assert_eq!(still.apply(&frame(seq, region, encoding, data), &mut picture, 8, 8), Ok(true));
        }
        let at = |x, y| pixel_at(&picture, x, y);
        assert_eq!([at(2, 1), at(3, 2), at(1, 1), at(0, 7)], [[9, 9, 9, 255], [9, 9, 9, 255], [1, 1, 1, 255], [0, 200, 0, 255]]);
        let [r, g, b, a] = at(5, 5);
        assert!(r > 180 && g < 30 && b < 30 && a == 255, "{:?}", [r, g, b, a]);
        assert_eq!(still.accepts(), [Encoding::Rgba, Encoding::Png, Encoding::Jpeg]);
        let outside = frame(9, rect(6, 6, 4, 4), Encoding::Rgba, RgbaImage::filled(4, 4, [0; 4]).data);
        assert!(still.apply(&outside, &mut picture, 8, 8).unwrap_err().0.contains("outside"));
    }

    #[test]
    fn documents_open_as_images_or_rendered_pages() {
        let mut book = rust_xlsxwriter::Workbook::new();
        book.add_worksheet().write_string(0, 0, "cell").unwrap();
        let xlsx = Document { name: "a.xlsx".into(), mime: Format::Xlsx.mime().into(), data: Bytes::from(book.save_to_buffer().unwrap()) };
        let png = Document { name: "a.png".into(), mime: "image/png".into(), data: RgbaImage::filled(30, 20, [1, 2, 3, 255]).to_png().unwrap().into() };
        let unknown = Document { name: "a.bin".into(), mime: "application/x-unknown".into(), data: Bytes::new() };
        let docs = DocsRenderer::new(Fonts::none());
        let palette = DocumentPalette { page: [255; 4], text: [0, 0, 0, 255], rule: [128; 4], grid: [217; 4], header: [242; 4], header_text: [64; 4] };

        let Ok(OpenedDocument::Paged(sheet)) = docs.open(&xlsx) else { panic!("the sheet opens as pages") };
        assert_eq!(sheet.page_count(), 1);
        assert!(sheet.page_size(0).is_ok());
        let page = sheet.render(0, 1.25, &palette).unwrap();
        assert!(page.width > 0 && page.height > 0);
        assert_eq!(page.data.len(), (page.width * page.height * 4) as usize);
        assert!(matches!(docs.open(&png), Ok(OpenedDocument::Image { width: 30, height: 20 })));
        assert_eq!(docs.open(&unknown).err().as_deref(), Some("no viewer for application/x-unknown"));
    }

    #[test]
    fn the_picker_lists_every_docs_format() {
        let extensions = DocsRenderer::new(Fonts::none()).extensions();
        for ext in ["pdf", "xlsx", "xlsm", "docx", "pptx", "png", "jpg", "jpeg", "gif", "webp", "bmp"] {
            assert!(extensions.iter().any(|e| e == ext), "{ext}");
        }
        assert_eq!(loa_view::surface::DOCUMENT_SERVER, loa_surfaces::docs::SERVER_NAME);
    }
}
