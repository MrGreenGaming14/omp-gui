//! The `docs` surface server: opens files as document mode surfaces.

use crate::docs::{DocsError, Fonts, Format, PageSize, Paged, Result, json, open_pages};
use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{
    Document, Frame, InputEvent, Result as SurfaceResult, Surface, SurfaceError, SurfaceInfo, SurfaceMode,
    SurfaceServer, SurfaceUri,
};
use loa_util::LockExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Server part of the `surface://docs/<id>` uris.
pub const SERVER_NAME: &str = "docs";

/// Scale of `surface_screenshot` pages: 1.5 px per point keeps 10 pt text legible for the agent while a Letter
/// page stays under 1200 px tall.
const SCREENSHOT_SCALE: f32 = 1.5;

impl From<DocsError> for SurfaceError {
    fn from(error: DocsError) -> Self {
        match error {
            DocsError::Surface(inner) => inner,
            other => SurfaceError::Backend(other.to_string()),
        }
    }
}

/// Structured data the agent tools ask a document surface for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DataQuery {
    /// Format, MIME type, page count and page sizes. Every format.
    Info,
    /// Page text (PDF, one entry per page or just `page`), document text (DOCX) or slide text (PPTX).
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

impl DataQuery {
    /// What the query asks for, for the error when a format lacks it.
    fn what(&self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Text { .. } => "page text",
            Self::Sheets => "sheets",
            Self::Cells { .. } => "cells",
            Self::Outline => "outline",
            Self::Slides => "slides",
        }
    }
}

/// Answer to [`DataQuery::Info`].
#[derive(Serialize)]
struct DocInfo<'a> {
    uri: &'a SurfaceUri,
    name: &'a str,
    path: &'a Path,
    format: Format,
    mime: &'static str,
    pages: Vec<PageSize>,
}

struct Inner {
    uri: SurfaceUri,
    name: String,
    path: PathBuf,
    format: Format,
    fonts: Fonts,
    bytes: Bytes,
    parsed: Box<dyn Paged>,
}

/// One open document. Cheap to clone.
#[derive(Clone)]
pub struct DocSurface(Arc<Inner>);

impl DocSurface {
    /// Answers a structured data query as JSON. A query the format doesn't offer is a [`DocsError::Request`].
    pub async fn data(&self, query: DataQuery) -> Result<Value> {
        let this = self.clone();
        run_blocking(move || this.answer(query)).await
    }

    fn answer(&self, query: DataQuery) -> Result<Value> {
        let Inner { uri, name, path, format, parsed, .. } = &*self.0;
        if let DataQuery::Info = query {
            let pages = (0..parsed.page_count()).map(|i| parsed.page_size(i)).collect::<Result<_>>()?;
            return json(&DocInfo { uri, name, path, format: *format, mime: format.mime(), pages });
        }
        parsed.query(&query).unwrap_or_else(|| Err(DocsError::Request(format!("{format} files have no {}", query.what()))))
    }

    fn mode_error(&self) -> SurfaceError {
        SurfaceError::Mode { uri: self.0.uri.clone(), mode: SurfaceMode::Document }
    }
}

async fn run_blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work).await.map_err(|e| DocsError::Surface(SurfaceError::Backend(e.to_string())))?
}

#[async_trait]
impl Surface for DocSurface {
    fn info(&self) -> SurfaceInfo {
        let Inner { uri, name, format, parsed, .. } = &*self.0;
        let (width, height) = parsed.page_size(0).map(|s| s.pixels(1.0)).unwrap_or((1, 1));
        let mime = Some(format.mime().to_owned());
        SurfaceInfo { uri: uri.clone(), name: name.clone(), width, height, mode: SurfaceMode::Document, mime }
    }

    fn frames(&self) -> SurfaceResult<broadcast::Receiver<Arc<Frame>>> {
        Err(self.mode_error())
    }

    async fn request_keyframe(&self) -> SurfaceResult<()> {
        Err(self.mode_error())
    }

    async fn input(&self, _event: InputEvent) -> SurfaceResult<()> {
        Err(self.mode_error())
    }

    async fn document(&self) -> SurfaceResult<Document> {
        Ok(Document { name: self.0.name.clone(), mime: self.0.format.mime().to_owned(), data: self.0.bytes.clone() })
    }

    /// The first page as PNG. Images are returned at their own size, a PNG file as it is.
    async fn screenshot(&self) -> SurfaceResult<Bytes> {
        if self.0.format == Format::Png {
            return Ok(self.0.bytes.clone());
        }
        let this = self.clone();
        let scale = if this.0.format.is_image() { 1.0 } else { SCREENSHOT_SCALE };
        let png = run_blocking(move || Ok(this.0.parsed.render_page(0, scale, &this.0.fonts)?.to_png()?)).await?;
        Ok(Bytes::from(png))
    }
}

/// The `docs` surface server. Files are opened explicitly with [`DocsServer::open_file`]; `list` returns the
/// open ones.
pub struct DocsServer {
    fonts: Fonts,
    next_id: AtomicU64,
    surfaces: Mutex<HashMap<SurfaceUri, DocSurface>>,
}

impl DocsServer {
    /// `fonts` draws text in XLSX, DOCX and PPTX renderings; pass [`Fonts::system`] in the app.
    pub fn new(fonts: Fonts) -> Self {
        Self { fonts, next_id: AtomicU64::new(1), surfaces: Mutex::new(HashMap::new()) }
    }

    /// Opens the file at `path` as `surface://docs/<id>`. The format comes from the extension, else from the
    /// file's magic bytes. The file is parsed now, so a broken file fails here.
    pub async fn open_file(&self, path: impl Into<PathBuf>) -> Result<DocSurface> {
        let path = path.into();
        let (format, bytes, parsed) = run_blocking({
            let path = path.clone();
            move || {
                let bytes = crate::docs::read_file(&path)?;
                let format =
                    Format::detect(&path, &bytes).ok_or_else(|| DocsError::Unsupported(path.display().to_string()))?;
                let parsed = open_pages(format, &bytes)?;
                Ok((format, Bytes::from(bytes), parsed))
            }
        })
        .await?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let uri = SurfaceUri::new(SERVER_NAME, &id.to_string())?;
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let inner = Inner { uri: uri.clone(), name, path, format, fonts: self.fonts.clone(), bytes, parsed };
        let surface = DocSurface(Arc::new(inner));
        self.surfaces.locked().insert(uri, surface.clone());
        Ok(surface)
    }

    /// Forgets an open surface. Returns whether it was open.
    pub fn close(&self, uri: &SurfaceUri) -> bool {
        self.surfaces.locked().remove(uri).is_some()
    }

    /// The open surface at `uri`, typed, for data queries.
    pub fn surface(&self, uri: &SurfaceUri) -> Result<DocSurface> {
        let found = self.surfaces.locked().get(uri).cloned();
        found.ok_or_else(|| DocsError::Surface(SurfaceError::NotFound(uri.clone())))
    }
}

#[async_trait]
impl SurfaceServer for DocsServer {
    fn name(&self) -> &str {
        SERVER_NAME
    }

    async fn list(&self) -> SurfaceResult<Vec<SurfaceInfo>> {
        let mut infos: Vec<SurfaceInfo> = self.surfaces.locked().values().map(Surface::info).collect();
        infos.sort_by(|a, b| a.uri.name().len().cmp(&b.uri.name().len()).then_with(|| a.uri.name().cmp(b.uri.name())));
        Ok(infos)
    }

    async fn open(&self, uri: &SurfaceUri) -> SurfaceResult<Arc<dyn Surface>> {
        Ok(Arc::new(self.surface(uri)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn broken_files_fail_at_open_and_open_ones_answer_queries_until_closed() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let server = DocsServer::new(Fonts::none());
        assert!(matches!(server.open_file(dir.path().join("missing.pdf")).await, Err(DocsError::Io { .. })));
        assert!(matches!(server.open_file(write("notes.txt", b"plain")).await, Err(DocsError::Unsupported(_))));
        assert!(server.open_file(write("broken.pdf", b"%PDF-1.7 nothing else")).await.is_err());
        assert!(server.list().await.unwrap().is_empty());

        let png = crate::docs::Rgba::filled(2, 2, [0, 0, 0, 255]).to_png().unwrap();
        let surface = server.open_file(write("a.png", &png)).await.unwrap();
        let uri = surface.info().uri;
        assert_eq!(uri.as_str(), "surface://docs/1");
        assert!(matches!(surface.data(DataQuery::Sheets).await, Err(DocsError::Request(_))));
        let info = surface.data(DataQuery::Info).await.unwrap();
        assert_eq!((info["format"].as_str(), info["pages"][0]["width"].as_f64()), (Some("png"), Some(2.0)));
        assert!(matches!(surface.frames(), Err(SurfaceError::Mode { .. })));
        assert_eq!(surface.screenshot().await.unwrap(), png);
        assert!(server.close(&uri));
        assert!(matches!(server.open(&uri).await, Err(SurfaceError::NotFound(_))));
    }
}
