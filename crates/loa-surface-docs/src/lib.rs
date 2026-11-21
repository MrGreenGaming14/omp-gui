//! Document surface servers: PDF, XLSX, DOCX, PPTX, images.
//!
//! [`DocsServer`] is the `docs` surface server. It opens a file as
//! `surface://docs/<id>`, hands the file over once through
//! [`Surface::document`](loa_contracts::surface::Surface::document), renders PNG
//! screenshots and answers the structured data queries the agent tools use
//! ([`DataQuery`]).
//!
//! Document mode renders on the viewing side, so each format also exposes a
//! page renderer the viewer calls directly: [`open_pages`] or the per format
//! types ([`pdf::PdfDoc`], [`xlsx::XlsxDoc`], [`docx::DocxDoc`],
//! [`pptx::PptxDoc`], [`images::ImageDoc`]), all implementing [`Paged`].

pub mod docx;
pub mod images;
pub mod pdf;
pub mod pptx;
pub mod raster;
pub mod server;
pub mod xlsx;

pub use raster::Fonts;
pub use server::{DataQuery, DocSurface, DocsServer, SERVER_NAME};

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum DocsError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid {format} file: {message}")]
    Invalid { format: Format, message: String },
    #[error("page {index} is out of range, the document has {count}")]
    PageRange { index: usize, count: usize },
    #[error("unsupported file type `{0}`")]
    Unsupported(String),
    /// A query that doesn't fit the document (unknown sheet, bad range).
    #[error("{0}")]
    Request(String),
    #[error("font: {0}")]
    Font(String),
    #[error("encoding png: {0}")]
    Png(String),
    #[error(transparent)]
    Surface(#[from] loa_contracts::surface::SurfaceError),
}

pub type Result<T, E = DocsError> = std::result::Result<T, E>;

impl DocsError {
    pub(crate) fn invalid(format: Format, message: impl std::fmt::Display) -> Self {
        Self::Invalid { format, message: message.to_string() }
    }

    pub(crate) fn page_range(index: usize, count: usize) -> Self {
        Self::PageRange { index, count }
    }
}

/// The file formats the `docs` server opens.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pdf,
    Xlsx,
    Docx,
    Pptx,
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
}

impl Format {
    pub const ALL: [Self; 9] = [
        Self::Pdf,
        Self::Xlsx,
        Self::Docx,
        Self::Pptx,
        Self::Png,
        Self::Jpeg,
        Self::Gif,
        Self::Webp,
        Self::Bmp,
    ];

    pub fn mime(self) -> &'static str {
        match self {
            Self::Pdf => "application/pdf",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Pptx => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
            Self::Bmp => "image/bmp",
        }
    }

    pub fn from_mime(mime: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.mime().eq_ignore_ascii_case(mime))
    }

    /// From a file extension, case insensitive.
    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.to_ascii_lowercase();
        Some(match ext.as_str() {
            "pdf" => Self::Pdf,
            "xlsx" | "xlsm" => Self::Xlsx,
            "docx" => Self::Docx,
            "pptx" => Self::Pptx,
            "png" => Self::Png,
            "jpg" | "jpeg" => Self::Jpeg,
            "gif" => Self::Gif,
            "webp" => Self::Webp,
            "bmp" => Self::Bmp,
            _ => return None,
        })
    }

    /// From the extension of `path`, falling back to the file's magic bytes.
    pub fn detect(path: &Path, bytes: &[u8]) -> Option<Self> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(Self::from_extension)
            .or_else(|| Self::sniff(bytes))
    }

    /// From magic bytes. Office files are zip archives told apart by their
    /// main part.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"%PDF-") {
            return Some(Self::Pdf);
        }
        if bytes.starts_with(b"PK\x03\x04") {
            let find = |needle: &[u8]| bytes.windows(needle.len()).any(|w| w == needle);
            return if find(b"xl/workbook.xml") {
                Some(Self::Xlsx)
            } else if find(b"word/document.xml") {
                Some(Self::Docx)
            } else if find(b"ppt/presentation.xml") {
                Some(Self::Pptx)
            } else {
                None
            };
        }
        match ::image::guess_format(bytes).ok()? {
            ::image::ImageFormat::Png => Some(Self::Png),
            ::image::ImageFormat::Jpeg => Some(Self::Jpeg),
            ::image::ImageFormat::Gif => Some(Self::Gif),
            ::image::ImageFormat::WebP => Some(Self::Webp),
            ::image::ImageFormat::Bmp => Some(Self::Bmp),
            _ => None,
        }
    }

    pub fn is_image(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg | Self::Gif | Self::Webp | Self::Bmp)
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pdf => "PDF",
            Self::Xlsx => "XLSX",
            Self::Docx => "DOCX",
            Self::Pptx => "PPTX",
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
            Self::Webp => "WebP",
            Self::Bmp => "BMP",
        })
    }
}

/// 8 bit RGBA pixels, straight alpha, row major, no padding (the layout of
/// [`loa_contracts::surface::Encoding::Rgba`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Rgba {
    /// Encodes the pixels as PNG.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        use ::image::ImageEncoder;
        let mut out = Vec::new();
        ::image::codecs::png::PngEncoder::new(&mut out)
            .write_image(&self.data, self.width, self.height, ::image::ExtendedColorType::Rgba8)
            .map_err(|e| DocsError::Png(e.to_string()))?;
        Ok(out)
    }

    /// The pixel at `(x, y)`.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        (x < self.width && y < self.height).then(|| {
            let i = (y as usize * self.width as usize + x as usize) * 4;
            [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
        })
    }
}

/// Size of a page at scale 1, in points (1/72 inch), which are also surface
/// pixels at scale 1.
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PageSize {
    pub width: f32,
    pub height: f32,
}

impl PageSize {
    /// Pixel size of the page rendered at `scale`, at least 1 x 1.
    pub fn pixels(self, scale: f32) -> (u32, u32) {
        let px = |v: f32| ((v * scale).round() as u32).max(1);
        (px(self.width), px(self.height))
    }
}

/// A document the viewer can page through and render locally. A page is a
/// PDF page, a sheet, a DOCX page, a slide or the image.
pub trait Paged: Send + Sync {
    fn page_count(&self) -> usize;

    /// Size of page `index` (from 0) at scale 1.
    fn page_size(&self, index: usize) -> Result<PageSize>;

    /// Renders page `index` at `scale` (1.0 = one pixel per point).
    fn render_page(&self, index: usize, scale: f32, fonts: &Fonts) -> Result<Rgba>;
}

/// Parses a document for local rendering on the viewing side.
pub fn open_pages(format: Format, bytes: &[u8]) -> Result<Box<dyn Paged>> {
    Ok(match format {
        Format::Pdf => Box::new(pdf::PdfDoc::parse(bytes)?),
        Format::Xlsx => Box::new(xlsx::XlsxDoc::parse(bytes)?),
        Format::Docx => Box::new(docx::DocxDoc::parse(bytes)?),
        Format::Pptx => Box::new(pptx::PptxDoc::parse(bytes)?),
        image => Box::new(images::ImageDoc::parse(image, bytes)?),
    })
}

/// Reads a file, mapping the error to [`DocsError::Io`].
pub(crate) fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| DocsError::Io { path: path.to_owned(), source })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_files_are_told_apart_without_an_extension() {
        let mut zip = b"PK\x03\x04....".to_vec();
        zip.extend_from_slice(b"word/document.xml");
        assert_eq!(Format::detect(Path::new("upload"), &zip), Some(Format::Docx));
        assert_eq!(Format::detect(Path::new("a.PDF"), b""), Some(Format::Pdf));
        assert_eq!(Format::sniff(b"%PDF-1.7"), Some(Format::Pdf));
        assert_eq!(Format::sniff(b"PK\x03\x04 other zip"), None);
    }
}
