//! [`PageRenderers`] backed by `loa-surface-docs`: PDF, XLSX, DOCX and PPTX
//! documents rendered on the viewing side.

use crate::document::{PageError, PageRenderer, PageRenderers, RgbaPage};
use iced::Size;
use loa_surface::Document;
use loa_surface_docs::{Fonts, Format, Paged, open_pages};

/// Opens documents with the `loa-surface-docs` parsers and renders their
/// pages with `fonts`.
#[derive(Clone, Debug)]
pub struct DocsRenderers {
    fonts: Fonts,
}

impl DocsRenderers {
    pub fn new(fonts: Fonts) -> Self {
        Self { fonts }
    }

    /// Renders with the fonts installed on this system.
    pub fn system() -> Self {
        Self::new(Fonts::system())
    }
}

impl PageRenderers for DocsRenderers {
    fn open(&self, document: &Document) -> Result<Box<dyn PageRenderer>, PageError> {
        let format = Format::from_mime(&document.mime)
            .or_else(|| Format::sniff(&document.data))
            .ok_or_else(|| PageError::Unsupported(document.mime.clone()))?;
        let paged = open_pages(format, &document.data).map_err(|e| PageError::Render(e.to_string()))?;
        Ok(Box::new(DocsPages { paged, fonts: self.fonts.clone() }))
    }
}

struct DocsPages {
    paged: Box<dyn Paged>,
    fonts: Fonts,
}

impl PageRenderer for DocsPages {
    fn page_count(&self) -> usize {
        self.paged.page_count()
    }

    fn page_size(&self, index: usize) -> Option<Size> {
        let size = self.paged.page_size(index).ok()?;
        Some(Size::new(size.width, size.height))
    }

    fn render(&self, index: usize, scale: f32) -> Result<RgbaPage, PageError> {
        let page = self.paged.render_page(index, scale, &self.fonts).map_err(|e| match e {
            loa_surface_docs::DocsError::PageRange { index, .. } => PageError::NoPage(index),
            other => PageError::Render(other.to_string()),
        })?;
        Ok(RgbaPage { width: page.width, height: page.height, pixels: page.data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn xlsx() -> Vec<u8> {
        let mut workbook = rust_xlsxwriter::Workbook::new();
        let sheet = workbook.add_worksheet();
        sheet.write_string(0, 0, "name").unwrap();
        sheet.write_number(1, 0, 41.0).unwrap();
        workbook.save_to_buffer().unwrap()
    }

    #[test]
    fn office_documents_open_and_render_pages() {
        let renderers = DocsRenderers::new(Fonts::none());
        let document = Document {
            name: "book.xlsx".into(),
            mime: Format::Xlsx.mime().into(),
            data: Bytes::from(xlsx()),
        };
        let pages = renderers.open(&document).unwrap();
        assert_eq!(pages.page_count(), 1);
        let size = pages.page_size(0).unwrap();
        let page = pages.render(0, 0.5).unwrap();
        assert_eq!(page.width, (size.width * 0.5).round() as u32);
        assert_eq!(page.pixels.len(), (page.width * page.height * 4) as usize);
        assert!(matches!(pages.render(5, 1.0), Err(PageError::NoPage(5))));

        let unknown = Document { name: "x".into(), mime: "text/plain".into(), data: Bytes::from_static(b"hi") };
        assert!(matches!(renderers.open(&unknown), Err(PageError::Unsupported(_))));
    }
}
