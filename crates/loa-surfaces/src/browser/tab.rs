//! One browser tab as a pixel surface.

use loa_util::LockExt;
use crate::feed::{FrameFeed, InputLog};
use loa_util::time::now_us;
use crate::browser::input::{Command, InputMapper};
use crate::browser::server::Instance;
use crate::browser::structured::{self, OutlineNode, PageInfo};
use crate::browser::{BrowserError, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use async_trait::async_trait;
use bytes::Bytes;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, CaptureScreenshotParams};
use loa_contracts::surface::{Document, Frame, InputEvent, Surface, SurfaceError, SurfaceInfo, SurfaceMode, SurfaceUri};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::broadcast;

/// Where a tab's frames come from: Chrome's screencast or a virtual display grab.
#[async_trait]
pub(crate) trait Capture: Send + Sync {
    /// The viewport the capture can show for a requested `width` x `height`.
    fn fit(&self, width: u32, height: u32) -> (u32, u32) {
        (width, height)
    }

    /// Sets the page viewport and captures at the new size.
    async fn resize(&self, page: &Page, width: u32, height: u32) -> Result<()>;

    /// Makes the next frame a whole picture; `subscribe` when a viewer just subscribed.
    fn keyframe(&self, hub: &FrameFeed, subscribe: bool);

    fn stop(&self);
}

/// A browser tab: `surface://browser/<tab>`.
pub struct Tab {
    uri: SurfaceUri,
    page: Page,
    /// Shared with the other tabs in screencast mode, owned in display mode.
    instance: Arc<Instance>,
    hub: Arc<FrameFeed>,
    log: Arc<Mutex<InputLog>>,
    size: Arc<Mutex<(u32, u32)>>,
    /// Held while an input is dispatched so inputs reach Chrome in order.
    mapper: tokio::sync::Mutex<InputMapper>,
    label: Mutex<String>,
    capture: Box<dyn Capture>,
}

/// Everything a tab is made of, put together by the server.
pub(crate) struct TabParts {
    pub uri: SurfaceUri,
    pub page: Page,
    pub instance: Arc<Instance>,
    pub hub: Arc<FrameFeed>,
    pub log: Arc<Mutex<InputLog>>,
    pub size: Arc<Mutex<(u32, u32)>>,
    pub capture: Box<dyn Capture>,
}

impl Tab {
    pub(crate) fn new(parts: TabParts) -> Self {
        Self {
            label: Mutex::new(parts.uri.name().to_owned()),
            uri: parts.uri,
            page: parts.page,
            instance: parts.instance,
            hub: parts.hub,
            log: parts.log,
            size: parts.size,
            mapper: tokio::sync::Mutex::new(InputMapper::default()),
            capture: parts.capture,
        }
    }

    pub fn uri(&self) -> &SurfaceUri {
        &self.uri
    }

    pub(crate) fn instance(&self) -> &Arc<Instance> {
        &self.instance
    }

    pub(crate) fn page(&self) -> &Page {
        &self.page
    }

    /// Loads `url` and waits for the load event.
    pub async fn navigate(&self, url: &str) -> Result<PageInfo> {
        self.page.goto(url).await?;
        self.page_info().await
    }

    /// Current URL and title. Also updates the name shown on the surface tab.
    pub async fn page_info(&self) -> Result<PageInfo> {
        let info = structured::page_info(&self.page).await?;
        let label = if info.title.is_empty() { &info.url } else { &info.title };
        label.clone_into(&mut self.label.locked());
        Ok(info)
    }

    /// Readable text of the page, without markup, scripts or hidden elements.
    pub async fn page_text(&self) -> Result<String> {
        structured::page_text(&self.page).await
    }

    /// Interactive elements with role, name and box in surface coordinates.
    pub async fn outline(&self) -> Result<Vec<OutlineNode>> {
        structured::outline(&self.page, self.size()).await
    }

    fn size(&self) -> (u32, u32) {
        *self.size.locked()
    }

    /// Dispatches one input. It is logged once Chrome has handled it, so a
    /// frame is only stamped with inputs whose effect it can contain.
    async fn dispatch(&self, event: &InputEvent) -> Result<()> {
        let mut mapper = self.mapper.lock().await;
        let Some(command) = mapper.map(&event.input, Instant::now()) else {
            return Ok(());
        };
        match command {
            Command::Mouse(params) => drop(self.page.execute(params).await?),
            Command::Key(params) => drop(self.page.execute(params).await?),
            Command::Text(params) => drop(self.page.execute(params).await?),
            Command::Resize { width, height } => self.resize(width, height).await?,
        }
        self.log.locked().record(event.seq, now_us());
        Ok(())
    }

    /// Sets the page viewport, clipped to what the capture can show.
    async fn resize(&self, width: u32, height: u32) -> Result<()> {
        let (width, height) = self.capture.fit(width, height);
        if (width, height) == self.size() {
            return Ok(());
        }
        self.capture.resize(&self.page, width, height).await?;
        *self.size.locked() = (width, height);
        Ok(())
    }

    /// Stops the frame feed. The page itself is closed by the server.
    pub(crate) fn stop_feed(&self) {
        self.capture.stop();
    }
}

impl Drop for Tab {
    fn drop(&mut self) {
        self.stop_feed();
    }
}

/// Emulates a `width` x `height` viewport at one device pixel per CSS pixel,
/// so surface coordinates are CSS pixels.
pub(crate) async fn set_viewport(page: &Page, width: u32, height: u32) -> Result<()> {
    page.execute(SetDeviceMetricsOverrideParams::new(i64::from(width), i64::from(height), 1.0, false)).await?;
    Ok(())
}

#[async_trait]
impl Surface for Tab {
    fn info(&self) -> SurfaceInfo {
        let (width, height) = self.size();
        SurfaceInfo {
            uri: self.uri.clone(),
            name: self.label.locked().clone(),
            width,
            height,
            mode: SurfaceMode::Pixel,
            mime: None,
        }
    }

    fn frames(&self) -> loa_contracts::surface::Result<broadcast::Receiver<Arc<Frame>>> {
        let receiver = self.hub.subscribe();
        self.capture.keyframe(&self.hub, true);
        Ok(receiver)
    }

    async fn request_keyframe(&self) -> loa_contracts::surface::Result<()> {
        self.capture.keyframe(&self.hub, false);
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> loa_contracts::surface::Result<()> {
        Ok(self.dispatch(&event).await?)
    }

    async fn document(&self) -> loa_contracts::surface::Result<Document> {
        Err(SurfaceError::Mode { uri: self.uri.clone(), mode: SurfaceMode::Pixel })
    }

    async fn screenshot(&self) -> loa_contracts::surface::Result<Bytes> {
        let params = CaptureScreenshotParams { format: Some(CaptureScreenshotFormat::Png), ..Default::default() };
        let shot = self.page.execute(params).await.map_err(BrowserError::from)?;
        let png = STANDARD.decode(shot.result.data.as_ref() as &str).map_err(|e| BrowserError::Script(e.to_string()))?;
        Ok(Bytes::from(png))
    }
}
