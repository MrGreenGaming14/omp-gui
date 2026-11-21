//! Document surfaces: PDF, XLSX, DOCX, PPTX and images.
//!
//! [`FORMATS`] registers each format (MIME type, extensions, magic, parser); a
//! parsed file is a [`Paged`] document the viewer renders locally and the
//! [`DocsServer`] (`surface://docs/<id>`) serves and queries.

pub mod docx;
pub mod images;
mod ooxml;
pub mod pdf;
pub mod pptx;
pub mod raster;
pub mod server;
pub mod xlsx;

pub use raster::{DocPalette, DocTypography, Fonts, RenderOpts};
pub use server::{DataQuery, DocSurface, DocsServer, SERVER_NAME};

use ::image::ImageFormat;
use serde_json::Value;
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
    #[error(transparent)]
    Pixels(#[from] crate::pixels::PixelError),
    #[error(transparent)]
    Surface(#[from] loa_contracts::surface::SurfaceError),
}

pub type Result<T, E = DocsError> = std::result::Result<T, E>;

impl DocsError {
    pub(crate) fn invalid(format: Format, message: impl std::fmt::Display) -> Self {
        Self::Invalid { format, message: message.to_string() }
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

/// How a file of a format is recognised by its content.
#[derive(Clone, Copy, Debug)]
pub enum Sniff {
    Magic(&'static [u8]),
    /// A zip archive holding this part.
    ZipPart(&'static [u8]),
    /// An image the `image` crate recognises.
    Image(ImageFormat),
}

pub type Opener = fn(Format, &[u8]) -> Result<Box<dyn Paged>>;

pub struct DocFormat {
    pub format: Format,
    pub name: &'static str,
    pub mime: &'static str,
    pub exts: &'static [&'static str],
    pub sniff: Sniff,
    pub open: Opener,
}

macro_rules! formats {
    ($($format:ident $name:literal $mime:expr, [$($ext:literal),*], $sniff:expr, $open:expr;)*) => {
        /// Every format, in sniffing order.
        pub static FORMATS: &[DocFormat] = &[$(DocFormat {
            format: Format::$format, name: $name, mime: $mime, exts: &[$($ext),*], sniff: $sniff, open: $open,
        }),*];
    };
}

/// The opener of a document type with `parse(&[u8])`.
macro_rules! parse {
    ($doc:ty) => {
        |_, bytes| Ok(Box::new(<$doc>::parse(bytes)?))
    };
}

formats! {
    Pdf "PDF" "application/pdf", ["pdf"], Sniff::Magic(b"%PDF-"), parse!(pdf::PdfDoc);
    Xlsx "XLSX" "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", ["xlsx", "xlsm"], Sniff::ZipPart(b"xl/workbook.xml"), parse!(xlsx::XlsxDoc);
    Docx "DOCX" "application/vnd.openxmlformats-officedocument.wordprocessingml.document", ["docx"], Sniff::ZipPart(b"word/document.xml"), parse!(docx::DocxDoc);
    Pptx "PPTX" "application/vnd.openxmlformats-officedocument.presentationml.presentation", ["pptx"], Sniff::ZipPart(b"ppt/presentation.xml"), parse!(pptx::PptxDoc);
    Png "PNG" "image/png", ["png"], Sniff::Image(ImageFormat::Png), images::open;
    Jpeg "JPEG" "image/jpeg", ["jpg", "jpeg"], Sniff::Image(ImageFormat::Jpeg), images::open;
    Gif "GIF" "image/gif", ["gif"], Sniff::Image(ImageFormat::Gif), images::open;
    Webp "WebP" "image/webp", ["webp"], Sniff::Image(ImageFormat::WebP), images::open;
    Bmp "BMP" "image/bmp", ["bmp"], Sniff::Image(ImageFormat::Bmp), images::open;
}

impl Format {
    pub fn spec(self) -> &'static DocFormat {
        FORMATS.iter().find(|f| f.format == self).expect("every format is registered")
    }

    pub fn mime(self) -> &'static str {
        self.spec().mime
    }

    pub fn from_mime(mime: &str) -> Option<Self> {
        FORMATS.iter().find(|f| f.mime.eq_ignore_ascii_case(mime)).map(|f| f.format)
    }

    /// From a file extension, case insensitive.
    pub fn from_extension(ext: &str) -> Option<Self> {
        FORMATS.iter().find(|f| f.exts.iter().any(|e| e.eq_ignore_ascii_case(ext))).map(|f| f.format)
    }

    /// From the extension of `path`, falling back to the file's magic bytes.
    pub fn detect(path: &Path, bytes: &[u8]) -> Option<Self> {
        path.extension().and_then(|e| e.to_str()).and_then(Self::from_extension).or_else(|| Self::sniff(bytes))
    }

    /// From magic bytes; Office files are zip archives told apart by their main part.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        let image = ::image::guess_format(bytes).ok();
        let zip = bytes.starts_with(b"PK\x03\x04");
        let matches = |sniff: Sniff| match sniff {
            Sniff::Magic(magic) => bytes.starts_with(magic),
            Sniff::ZipPart(part) => zip && bytes.windows(part.len()).any(|w| w == part),
            Sniff::Image(format) => image == Some(format),
        };
        FORMATS.iter().find(|f| matches(f.sniff)).map(|f| f.format)
    }

    pub fn is_image(self) -> bool {
        matches!(self.spec().sniff, Sniff::Image(_))
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.spec().name)
    }
}

pub use crate::pixels::RgbaImage as Rgba;

/// Size of a page at scale 1, in points (1/72 inch), which are also surface pixels at scale 1.
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

/// A parsed document: pages (a PDF page, a sheet, a DOCX page, a slide or the image) and structured data.
pub trait Paged: Send + Sync {
    fn page_count(&self) -> usize;

    /// Size of page `index` (from 0) at scale 1.
    fn page_size(&self, index: usize) -> Result<PageSize>;

    /// Page count for the supplied rendering options.
    fn page_count_with_opts(&self, _opts: &RenderOpts) -> usize {
        self.page_count()
    }

    /// Page size for the supplied rendering options.
    fn page_size_with_opts(&self, index: usize, _opts: &RenderOpts) -> Result<PageSize> {
        self.page_size(index)
    }

    /// Renders page `index` at `scale` (1.0 = one pixel per point) with `opts`.
    fn render(&self, index: usize, scale: f32, fonts: &Fonts, opts: &RenderOpts) -> Result<Rgba>;

    fn render_page(&self, index: usize, scale: f32, fonts: &Fonts) -> Result<Rgba> {
        self.render(index, scale, fonts, &RenderOpts::default())
    }

    /// Answers `query`, or `None` when the format has no such data.
    fn query(&self, _query: &DataQuery) -> Option<Result<Value>> {
        None
    }
}

pub(crate) fn json<T: serde::Serialize + ?Sized>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| DocsError::Request(e.to_string()))
}

/// Page `index` of `pages`, or [`DocsError::PageRange`].
pub(crate) fn page_at<T>(pages: &[T], index: usize) -> Result<&T> {
    pages.get(index).ok_or(DocsError::PageRange { index, count: pages.len() })
}

/// Parses a document for local rendering on the viewing side.
pub fn open_pages(format: Format, bytes: &[u8]) -> Result<Box<dyn Paged>> {
    (format.spec().open)(format, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_are_detected_by_extension_mime_and_magic() {
        let mut zip = b"PK\x03\x04....".to_vec();
        zip.extend_from_slice(b"word/document.xml");
        let png = crate::pixels::encode_png(1, 1, &[0; 4]).unwrap();
        let cases: &[(&str, &[u8], Option<Format>)] = &[
            ("upload", &zip, Some(Format::Docx)),
            ("a.PDF", b"", Some(Format::Pdf)),
            ("b.xlsm", b"", Some(Format::Xlsx)),
            ("c.jpeg", b"", Some(Format::Jpeg)),
            ("noext", b"%PDF-1.7", Some(Format::Pdf)),
            ("noext", b"PK\x03\x04 other zip", None),
            ("noext", &png, Some(Format::Png)),
            ("notes.txt", b"plain", None),
        ];
        for (path, bytes, want) in cases {
            assert_eq!(Format::detect(Path::new(path), bytes), *want, "{path}");
        }
        for spec in FORMATS {
            assert_eq!(Format::from_mime(&spec.mime.to_uppercase()), Some(spec.format));
            assert_eq!(spec.format.to_string(), spec.name);
        }
        assert!(Format::Gif.is_image() && !Format::Pptx.is_image());
    }
}
