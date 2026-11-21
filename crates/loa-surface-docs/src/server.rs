//! The `docs` surface server: opens files as document mode surfaces.

use loa_util::{LockExt, RwLockExt};
use crate::docx::DocxDoc;
use crate::images::ImageDoc;
use crate::pdf::PdfDoc;
use crate::pptx::PptxDoc;
use crate::xlsx::XlsxDoc;
use crate::{DocsError, Fonts, Format, PageSize, Paged, Result, Rgba};
use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{
    Document, Frame, InputEvent, Surface, SurfaceError, SurfaceInfo, SurfaceMode, SurfaceServer, SurfaceUri,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::broadcast;

/// Server part of the `surface://docs/<id>` uris.
pub const SERVER_NAME: &str = "docs";

/// Scale of `surface_screenshot` pages: 1.5 px per point keeps 10 pt text
/// legible for the agent while a Letter page stays under 1200 px tall.
pub const SCREENSHOT_SCALE: f32 = 1.5;

impl From<DocsError> for SurfaceError {
    fn from(error: DocsError) -> Self {
        match error {
            DocsError::Surface(inner) => inner,
            other => SurfaceError::Backend(other.to_string()),
        }
    }
}

/// Structured data the agent tools ask a document surface for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataQuery {
    /// Format, MIME type, page count and page sizes. Every format.
    Info,
    /// Page text (PDF, one entry per page or just `page`), document text
    /// (DOCX) or slide text (PPTX).
    Text {
        #[serde(default)]
        page: Option<usize>,
    },
    /// XLSX: sheet names and used sizes.
    Sheets,
    /// XLSX: cells of `range` (A1 notation) in `sheet` (default: the first).
    Cells {
        #[serde(default)]
        sheet: Option<String>,
        range: String,
    },
    /// DOCX: paragraphs with style, heading level, list level and runs, and tables.
    Outline,
    /// PPTX: slides in presentation order with their shapes, text and notes.
    Slides,
}

/// Answer to [`DataQuery::Info`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DocInfo {
    pub uri: SurfaceUri,
    pub name: String,
    pub path: PathBuf,
    pub format: Format,
    pub mime: &'static str,
    pub pages: Vec<PageSize>,
}

/// A parsed document of any supported format.
enum Parsed {
    Pdf(PdfDoc),
    Xlsx(XlsxDoc),
    Docx(DocxDoc),
    Pptx(PptxDoc),
    Image(ImageDoc),
}

impl Parsed {
    fn parse(format: Format, bytes: &[u8]) -> Result<Self> {
        Ok(match format {
            Format::Pdf => Self::Pdf(PdfDoc::parse(bytes)?),
            Format::Xlsx => Self::Xlsx(XlsxDoc::parse(bytes)?),
            Format::Docx => Self::Docx(DocxDoc::parse(bytes)?),
            Format::Pptx => Self::Pptx(PptxDoc::parse(bytes)?),
            image => Self::Image(ImageDoc::parse(image, bytes)?),
        })
    }

    fn paged(&self) -> &dyn Paged {
        match self {
            Self::Pdf(doc) => doc,
            Self::Xlsx(doc) => doc,
            Self::Docx(doc) => doc,
            Self::Pptx(doc) => doc,
            Self::Image(doc) => doc,
        }
    }
}

/// The file contents and their parsed form, replaced as a whole on reload.
struct Loaded {
    bytes: Bytes,
    parsed: Parsed,
}

impl Loaded {
    fn read(path: &Path, format: Format) -> Result<Self> {
        let bytes = Bytes::from(crate::read_file(path)?);
        let parsed = Parsed::parse(format, &bytes)?;
        Ok(Self { bytes, parsed })
    }
}

struct Inner {
    uri: SurfaceUri,
    name: String,
    path: PathBuf,
    format: Format,
    fonts: Fonts,
    loaded: RwLock<Arc<Loaded>>,
}

/// One open document. Cheap to clone.
#[derive(Clone)]
pub struct DocSurface(Arc<Inner>);

impl DocSurface {
    fn loaded(&self) -> Arc<Loaded> {
        self.0.loaded.read_ok().clone()
    }

    pub fn uri(&self) -> &SurfaceUri {
        &self.0.uri
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }

    pub fn format(&self) -> Format {
        self.0.format
    }

    /// Reads the file again, e.g. after it changed on disk. The viewer must
    /// fetch [`Surface::document`] again to see the change.
    pub fn reload(&self) -> Result<()> {
        let loaded = Loaded::read(&self.0.path, self.0.format)?;
        *self.0.loaded.write_ok() = Arc::new(loaded);
        Ok(())
    }

    /// Renders page `index` (from 0) at `scale` as PNG, for a screenshot of a
    /// page other than the first.
    pub async fn screenshot_page(&self, index: usize, scale: f32) -> Result<Bytes> {
        let loaded = self.loaded();
        let fonts = self.0.fonts.clone();
        run_blocking(move || {
            if let Parsed::Image(image) = &loaded.parsed
                && index == 0
                && scale == 1.0
            {
                return image.png(&loaded.bytes);
            }
            loaded.parsed.paged().render_page(index, scale, &fonts)?.to_png()
        })
        .await
        .map(Bytes::from)
    }

    /// Renders page `index` to RGBA, the same picture the viewer draws.
    pub async fn render_page(&self, index: usize, scale: f32) -> Result<Rgba> {
        let loaded = self.loaded();
        let fonts = self.0.fonts.clone();
        run_blocking(move || loaded.parsed.paged().render_page(index, scale, &fonts)).await
    }

    /// Answers a structured data query as JSON. A query the format doesn't
    /// offer is a [`DocsError::Request`].
    pub async fn data(&self, query: DataQuery) -> Result<Value> {
        let loaded = self.loaded();
        let surface = self.clone();
        run_blocking(move || surface.answer(&loaded.parsed, query)).await
    }

    fn answer(&self, parsed: &Parsed, query: DataQuery) -> Result<Value> {
        let format = self.0.format;
        let unsupported = |what: &str| Err(DocsError::Request(format!("{format} files have no {what}")));
        match (query, parsed) {
            (DataQuery::Info, parsed) => {
                let paged = parsed.paged();
                let pages = (0..paged.page_count()).map(|i| paged.page_size(i)).collect::<Result<_>>()?;
                json(&DocInfo {
                    uri: self.0.uri.clone(),
                    name: self.0.name.clone(),
                    path: self.0.path.clone(),
                    format,
                    mime: format.mime(),
                    pages,
                })
            }
            (DataQuery::Text { page: Some(index) }, Parsed::Pdf(pdf)) => json(&pdf.page_text(index)?),
            (DataQuery::Text { page: None }, Parsed::Pdf(pdf)) => json(&pdf.text()?),
            (DataQuery::Text { page: None }, Parsed::Docx(docx)) => json(&docx.text()),
            (DataQuery::Text { page: Some(index) }, Parsed::Pptx(pptx)) => {
                let count = pptx.slides().len();
                json(pptx.slides().get(index).ok_or_else(|| DocsError::page_range(index, count))?)
            }
            (DataQuery::Text { page: None }, Parsed::Pptx(pptx)) => json(&pptx.text()),
            (DataQuery::Text { .. }, _) => unsupported("page text"),
            (DataQuery::Sheets, Parsed::Xlsx(xlsx)) => json(&xlsx.sheets()),
            (DataQuery::Sheets, _) => unsupported("sheets"),
            (DataQuery::Cells { sheet, range }, Parsed::Xlsx(xlsx)) => json(&xlsx.cells(sheet.as_deref(), &range)?),
            (DataQuery::Cells { .. }, _) => unsupported("cells"),
            (DataQuery::Outline, Parsed::Docx(docx)) => json(docx.outline()),
            (DataQuery::Outline, _) => unsupported("outline"),
            (DataQuery::Slides, Parsed::Pptx(pptx)) => json(pptx.slides()),
            (DataQuery::Slides, _) => unsupported("slides"),
        }
    }

    fn mode_error(&self) -> SurfaceError {
        SurfaceError::Mode { uri: self.0.uri.clone(), mode: SurfaceMode::Document }
    }
}

fn json<T: Serialize + ?Sized>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| DocsError::Request(e.to_string()))
}

/// Runs CPU bound parsing and rendering off the async workers.
async fn run_blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| DocsError::Surface(SurfaceError::Backend(e.to_string())))?
}

#[async_trait]
impl Surface for DocSurface {
    fn info(&self) -> SurfaceInfo {
        let loaded = self.loaded();
        let (width, height) = loaded.parsed.paged().page_size(0).map(|s| s.pixels(1.0)).unwrap_or((1, 1));
        SurfaceInfo {
            uri: self.0.uri.clone(),
            name: self.0.name.clone(),
            width,
            height,
            mode: SurfaceMode::Document,
            mime: Some(self.0.format.mime().to_owned()),
        }
    }

    fn frames(&self) -> loa_contracts::surface::Result<broadcast::Receiver<Arc<Frame>>> {
        Err(self.mode_error())
    }

    async fn request_keyframe(&self) -> loa_contracts::surface::Result<()> {
        Err(self.mode_error())
    }

    async fn input(&self, _event: InputEvent) -> loa_contracts::surface::Result<()> {
        Err(self.mode_error())
    }

    async fn document(&self) -> loa_contracts::surface::Result<Document> {
        Ok(Document {
            name: self.0.name.clone(),
            mime: self.0.format.mime().to_owned(),
            data: self.loaded().bytes.clone(),
        })
    }

    /// The first page as PNG. Images are returned at their own size.
    async fn screenshot(&self) -> loa_contracts::surface::Result<Bytes> {
        let scale = if self.0.format.is_image() { 1.0 } else { SCREENSHOT_SCALE };
        Ok(self.screenshot_page(0, scale).await?)
    }
}

/// The `docs` surface server. Files are opened explicitly with
/// [`DocsServer::open_file`]; `list` returns the open ones.
pub struct DocsServer {
    fonts: Fonts,
    next_id: AtomicU64,
    surfaces: Mutex<HashMap<SurfaceUri, DocSurface>>,
}

impl DocsServer {
    /// `fonts` draws text in XLSX, DOCX and PPTX renderings; pass
    /// [`Fonts::system`] in the app.
    pub fn new(fonts: Fonts) -> Self {
        Self { fonts, next_id: AtomicU64::new(1), surfaces: Mutex::new(HashMap::new()) }
    }

    /// Opens the file at `path` as `surface://docs/<id>`. The format comes
    /// from the extension, else from the file's magic bytes. The file is
    /// parsed now, so a broken file fails here.
    pub async fn open_file(&self, path: impl Into<PathBuf>) -> Result<DocSurface> {
        let path = path.into();
        let (format, loaded) = run_blocking({
            let path = path.clone();
            move || {
                let bytes = crate::read_file(&path)?;
                let format = Format::detect(&path, &bytes)
                    .ok_or_else(|| DocsError::Unsupported(path.display().to_string()))?;
                let parsed = Parsed::parse(format, &bytes)?;
                Ok((format, Loaded { bytes: Bytes::from(bytes), parsed }))
            }
        })
        .await?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let uri = SurfaceUri::new(SERVER_NAME, &id.to_string())?;
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let surface = DocSurface(Arc::new(Inner {
            uri: uri.clone(),
            name,
            path,
            format,
            fonts: self.fonts.clone(),
            loaded: RwLock::new(Arc::new(loaded)),
        }));
        self.surfaces.locked().insert(uri, surface.clone());
        Ok(surface)
    }

    /// Forgets an open surface. Returns whether it was open.
    pub fn close(&self, uri: &SurfaceUri) -> bool {
        self.surfaces.locked().remove(uri).is_some()
    }

    /// The open surface at `uri`, typed, for data queries and edits.
    pub fn surface(&self, uri: &SurfaceUri) -> Result<DocSurface> {
        self.surfaces.locked()
            .get(uri)
            .cloned()
            .ok_or_else(|| DocsError::Surface(SurfaceError::NotFound(uri.clone())))
    }
}

#[async_trait]
impl SurfaceServer for DocsServer {
    fn name(&self) -> &str {
        SERVER_NAME
    }

    async fn list(&self) -> loa_contracts::surface::Result<Vec<SurfaceInfo>> {
        let surfaces: Vec<DocSurface> =
            self.surfaces.locked().values().cloned().collect();
        let mut infos: Vec<SurfaceInfo> = surfaces.iter().map(Surface::info).collect();
        infos.sort_by(|a, b| a.uri.name().len().cmp(&b.uri.name().len()).then_with(|| a.uri.name().cmp(b.uri.name())));
        Ok(infos)
    }

    async fn open(&self, uri: &SurfaceUri) -> loa_contracts::surface::Result<Arc<dyn Surface>> {
        Ok(Arc::new(self.surface(uri)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::image::{ImageFormat, Rgba as Px, RgbaImage};
    use std::io::Cursor;

    fn png(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        RgbaImage::from_pixel(width, height, Px(color)).write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[tokio::test]
    async fn an_opened_file_is_listed_and_served_once_as_a_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("photo.png");
        let bytes = png(30, 20, [10, 200, 30, 255]);
        std::fs::write(&path, &bytes).unwrap();
        let server = DocsServer::new(Fonts::none());
        let opened = server.open_file(&path).await.unwrap();
        assert_eq!(opened.uri().as_str(), "surface://docs/1");

        let listed = server.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        let info = &listed[0];
        assert_eq!((info.width, info.height, info.mode), (30, 20, SurfaceMode::Document));
        assert_eq!(info.mime.as_deref(), Some("image/png"));
        assert_eq!(info.name, "photo.png");

        let surface = server.open(&info.uri).await.unwrap();
        let document = surface.document().await.unwrap();
        assert_eq!(document.data.as_ref(), bytes.as_slice());
        assert!(matches!(surface.frames(), Err(SurfaceError::Mode { .. })));
        let shot = ::image::load_from_memory(&surface.screenshot().await.unwrap()).unwrap().into_rgba8();
        assert_eq!(shot.dimensions(), (30, 20));
        assert_eq!(shot.get_pixel(5, 5).0, [10, 200, 30, 255]);

        assert!(server.close(&info.uri));
        assert!(matches!(server.open(&info.uri).await, Err(SurfaceError::NotFound(_))));
    }

    #[tokio::test]
    async fn unreadable_or_unknown_files_fail_at_open() {
        let dir = tempfile::tempdir().unwrap();
        let server = DocsServer::new(Fonts::none());
        assert!(matches!(server.open_file(dir.path().join("missing.pdf")).await, Err(DocsError::Io { .. })));
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "plain").unwrap();
        assert!(matches!(server.open_file(&text).await, Err(DocsError::Unsupported(_))));
        let broken = dir.path().join("broken.pdf");
        std::fs::write(&broken, "%PDF-1.7 nothing else").unwrap();
        assert!(server.open_file(&broken).await.is_err());
        assert!(server.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn queries_a_format_does_not_offer_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.png");
        std::fs::write(&path, png(2, 2, [0, 0, 0, 255])).unwrap();
        let surface = DocsServer::new(Fonts::none()).open_file(&path).await.unwrap();
        assert!(matches!(surface.data(DataQuery::Sheets).await, Err(DocsError::Request(_))));
        let info = surface.data(DataQuery::Info).await.unwrap();
        assert_eq!(info["format"], "png");
        assert_eq!(info["pages"][0]["width"], 2.0);
    }
}
