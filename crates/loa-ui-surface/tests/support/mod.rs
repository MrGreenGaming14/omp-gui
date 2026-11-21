//! Fakes at the renderer's port boundary: a scripted surface, a still decoder and a document
//! renderer, so the UI is tested without the app's media code.
#![allow(dead_code)]

use async_trait::async_trait;
use bytes::Bytes;
use loa_ui_surface::Options;
use loa_view::surface::port::{DecodeError, Decoders, DocumentRenderer, FrameDecoder, OpenedDocument, PageSize, PagedDocument, RgbaPage, DocumentPalette};
use loa_view::surface::{Document, Encoding, Frame, InputEvent, Surface, SurfaceError, SurfaceInfo, SurfaceMode};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{broadcast, mpsc};

/// A scripted surface: the test publishes frames through its [`Probe`] and reads back the
/// inputs and keyframe requests the code under test sent.
pub struct FakeSurface {
    pub info: SurfaceInfo,
    pub document: Option<Document>,
    frames: broadcast::Sender<Arc<Frame>>,
    inputs: mpsc::UnboundedSender<InputEvent>,
    keyframes: AtomicUsize,
}

/// The test's end of a [`FakeSurface`].
pub struct Probe {
    pub frames: broadcast::Sender<Arc<Frame>>,
    pub inputs: mpsc::UnboundedReceiver<InputEvent>,
}

impl FakeSurface {
    pub fn new(info: SurfaceInfo, document: Option<Document>) -> (Arc<Self>, Probe) {
        let frames = broadcast::channel(64).0;
        let (inputs_tx, inputs) = mpsc::unbounded_channel();
        let surface = Self { info, document, frames: frames.clone(), inputs: inputs_tx, keyframes: AtomicUsize::new(0) };
        (Arc::new(surface), Probe { frames, inputs })
    }

    pub fn keyframes_requested(&self) -> usize {
        self.keyframes.load(Ordering::SeqCst)
    }

    fn mode_error(&self) -> SurfaceError {
        SurfaceError::Mode { uri: self.info.uri.clone(), mode: self.info.mode }
    }
}

#[async_trait]
impl Surface for FakeSurface {
    fn info(&self) -> SurfaceInfo {
        self.info.clone()
    }

    fn frames(&self) -> Result<broadcast::Receiver<Arc<Frame>>, SurfaceError> {
        if self.info.mode != SurfaceMode::Pixel {
            return Err(self.mode_error());
        }
        Ok(self.frames.subscribe())
    }

    async fn request_keyframe(&self) -> Result<(), SurfaceError> {
        self.keyframes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> Result<(), SurfaceError> {
        let _ = self.inputs.send(event);
        Ok(())
    }

    async fn document(&self) -> Result<Document, SurfaceError> {
        self.document.clone().ok_or_else(|| self.mode_error())
    }

    async fn screenshot(&self) -> Result<Bytes, SurfaceError> {
        Ok(Bytes::new())
    }
}

/// `width` x `height` RGBA pixels of one colour.
pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.repeat(width as usize * height as usize)
}

/// Stills whose PNG and JPEG "codecs" carry the region's RGBA as is.
pub struct FakeStill;

impl FrameDecoder for FakeStill {
    fn accepts(&self) -> &[Encoding] {
        &[Encoding::Rgba, Encoding::Png, Encoding::Jpeg]
    }

    fn apply(&mut self, frame: &Frame, picture: &mut [u8], width: u32, height: u32) -> Result<bool, DecodeError> {
        let r = frame.region;
        if r.x + r.width > width || r.y + r.height > height || frame.data.len() != (r.width * r.height * 4) as usize {
            return Err(DecodeError("region doesn't fit".into()));
        }
        let row = (r.width * 4) as usize;
        for (y, src) in frame.data.chunks(row).enumerate() {
            let at = (((r.y + y as u32) * width + r.x) * 4) as usize;
            picture[at..at + row].copy_from_slice(src);
        }
        Ok(true)
    }
}

/// MIME type [`FakeDocuments`] opens as one 300x200 page.
pub const PAGED_MIME: &str = "application/x-fake-pages";

/// Opens `image/*` as a 30x20 image and [`PAGED_MIME`] as one page; nothing else.
pub struct FakeDocuments;

struct OnePage;

impl PagedDocument for OnePage {
    fn page_count(&self) -> usize {
        1
    }

    fn page_size(&self, _: usize) -> Result<PageSize, String> {
        Ok(PageSize { width: 300.0, height: 200.0 })
    }

    fn render(&self, _: usize, zoom: f32, palette: &DocumentPalette) -> Result<RgbaPage, String> {
        let (width, height) = ((300.0 * zoom) as u32, (200.0 * zoom) as u32);
        Ok(RgbaPage { width, height, data: solid(width, height, palette.page) })
    }
}

impl DocumentRenderer for FakeDocuments {
    fn extensions(&self) -> Vec<String> {
        ["pdf", "xlsx", "docx", "pptx"].map(String::from).to_vec()
    }

    fn open(&self, document: &Document) -> Result<OpenedDocument, String> {
        match document.mime.as_str() {
            mime if mime.starts_with("image/") => Ok(OpenedDocument::Image { width: 30, height: 20 }),
            PAGED_MIME => Ok(OpenedDocument::Paged(Box::new(OnePage))),
            mime => Err(format!("no viewer for {mime}")),
        }
    }
}

/// Options over the fakes, with no extra decoder.
pub fn options() -> Options {
    Options::new(Decoders { extra: None, still: Arc::new(|| Box::new(FakeStill)) }, Arc::new(FakeDocuments))
}
