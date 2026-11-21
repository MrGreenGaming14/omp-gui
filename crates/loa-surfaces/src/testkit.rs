//! Test doubles and helpers for surface code (feature `testkit`).

use async_trait::async_trait;
use bytes::Bytes;
use loa_contracts::surface::{
    Document, Frame, InputEvent, Rect, Result, Surface, SurfaceError, SurfaceInfo, SurfaceMode, SurfaceUri,
};
use loa_util::LockExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::mpsc;

/// Upper bound for anything a test waits on.
pub const PATIENCE: Duration = Duration::from_secs(20);

/// A scripted surface: the test publishes frames through its [`Probe`] and
/// reads back the inputs and keyframe requests the code under test sent.
pub struct FakeSurface {
    pub info: SurfaceInfo,
    pub document: Option<Document>,
    frames: Mutex<Option<broadcast::Sender<Arc<Frame>>>>,
    inputs: mpsc::UnboundedSender<InputEvent>,
    keyframes: mpsc::UnboundedSender<()>,
    keyframe_count: AtomicUsize,
}

/// The test's end of a [`FakeSurface`].
pub struct Probe {
    pub frames: broadcast::Sender<Arc<Frame>>,
    pub inputs: mpsc::UnboundedReceiver<InputEvent>,
    pub keyframes: mpsc::UnboundedReceiver<()>,
}

impl FakeSurface {
    pub fn new(info: SurfaceInfo, document: Option<Document>) -> (Arc<Self>, Probe) {
        let frames = broadcast::channel(64).0;
        let (inputs_tx, inputs) = mpsc::unbounded_channel();
        let (keyframes_tx, keyframes) = mpsc::unbounded_channel();
        let surface = Self {
            info,
            document,
            frames: Mutex::new(Some(frames.clone())),
            inputs: inputs_tx,
            keyframes: keyframes_tx,
            keyframe_count: AtomicUsize::new(0),
        };
        (Arc::new(surface), Probe { frames, inputs, keyframes })
    }

    /// A pixel surface `surface://<server>/<name>`.
    pub fn pixel(server: &str, name: &str, width: u32, height: u32) -> (Arc<Self>, Probe) {
        Self::new(info(server, name, width, height, SurfaceMode::Pixel, None), None)
    }

    /// A document surface `surface://<server>/<name>` serving `document`.
    pub fn document(server: &str, name: &str, document: Document) -> (Arc<Self>, Probe) {
        let info = info(server, name, 8, 4, SurfaceMode::Document, Some(document.mime.clone()));
        Self::new(info, Some(document))
    }

    /// Ends the frame stream for viewers subscribing from now on.
    pub fn close(&self) {
        self.frames.locked().take();
    }

    pub fn keyframes_requested(&self) -> usize {
        self.keyframe_count.load(Ordering::SeqCst)
    }

    fn mode_error(&self) -> SurfaceError {
        SurfaceError::Mode { uri: self.info.uri.clone(), mode: self.info.mode }
    }
}

fn info(server: &str, name: &str, width: u32, height: u32, mode: SurfaceMode, mime: Option<String>) -> SurfaceInfo {
    SurfaceInfo { uri: SurfaceUri::new(server, name).expect("valid uri"), name: name.into(), width, height, mode, mime }
}

#[async_trait]
impl Surface for FakeSurface {
    fn info(&self) -> SurfaceInfo {
        self.info.clone()
    }

    fn frames(&self) -> Result<broadcast::Receiver<Arc<Frame>>> {
        if self.info.mode != SurfaceMode::Pixel {
            return Err(self.mode_error());
        }
        self.frames.locked().as_ref().map(broadcast::Sender::subscribe).ok_or(SurfaceError::Closed)
    }

    async fn request_keyframe(&self) -> Result<()> {
        self.keyframe_count.fetch_add(1, Ordering::SeqCst);
        let _ = self.keyframes.send(());
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> Result<()> {
        let _ = self.inputs.send(event);
        Ok(())
    }

    async fn document(&self) -> Result<Document> {
        self.document.clone().ok_or_else(|| self.mode_error())
    }

    async fn screenshot(&self) -> Result<Bytes> {
        Ok(Bytes::new())
    }
}

/// Next frame within [`PATIENCE`], skipping over lag.
pub async fn next_frame(frames: &mut broadcast::Receiver<Arc<Frame>>) -> Arc<Frame> {
    loop {
        match tokio::time::timeout(PATIENCE, frames.recv()).await.expect("no frame in time") {
            Ok(frame) => return frame,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => panic!("frame stream closed"),
        }
    }
}

/// `width` x `height` RGBA pixels of one colour.
pub fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.repeat(width as usize * height as usize)
}

/// A full picture RGBA keyframe.
pub fn rgba_frame(seq: u64, width: u32, height: u32, rgba: [u8; 4]) -> Frame {
    Frame {
        seq,
        captured_us: seq,
        input_seq: 0,
        width,
        height,
        region: Rect { x: 0, y: 0, width, height },
        encoding: loa_contracts::surface::Encoding::Rgba,
        keyframe: true,
        data: solid(width, height, rgba).into(),
    }
}

/// `data:` URL of an HTML document.
pub fn html_data_url(body: &str) -> String {
    format!("data:text/html,{}", loa_util::text::percent_encode(body))
}

/// The Chrome for real-browser tests, which run only when `LOA_CHROME` is
/// set: to the browser's path, or to anything else to find the installed one.
#[cfg(feature = "browser")]
pub fn chrome() -> Option<std::path::PathBuf> {
    let wanted = std::env::var_os("LOA_CHROME")?;
    let path = std::path::PathBuf::from(&wanted);
    if path.is_file() {
        return Some(path);
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let found = crate::browser::find_chrome(std::env::var_os("PATH").as_deref(), home.as_deref(), false);
    assert!(found.is_some(), "LOA_CHROME is set but no Chrome or Chromium is installed");
    found
}

/// The Chrome for a real-browser test, or returns from the test with a
/// loud skip note when `LOA_CHROME` is unset.
#[cfg(feature = "browser")]
#[macro_export]
macro_rules! chrome_or_skip {
    () => {
        match $crate::testkit::chrome() {
            Some(chrome) => chrome,
            None => {
                eprintln!("SKIPPED: real-Chrome test; set LOA_CHROME=1 (or to a browser path) to run it");
                return;
            }
        }
    };
}
