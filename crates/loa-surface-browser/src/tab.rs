//! One browser tab as a pixel surface.

use crate::frames::{CaptureTarget, FrameHub, InputLog, now_us};
use crate::input::{Command, InputMapper};
use crate::server::Instance;
use crate::structured::{self, OutlineNode, PageInfo};
use crate::{BrowserError, Result, screencast};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use async_trait::async_trait;
use bytes::Bytes;
use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
use chromiumoxide::cdp::browser_protocol::page::{CaptureScreenshotFormat, CaptureScreenshotParams};
use loa_surface::{Document, Frame, InputEvent, Surface, SurfaceError, SurfaceInfo, SurfaceMode, SurfaceUri};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Where a tab's frames come from.
pub(crate) enum Feed {
    Screencast { pump: JoinHandle<()>, quality: u8 },
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Display { target: Arc<CaptureTarget>, screen: (u32, u32) },
}

/// A browser tab: `surface://browser/<tab>`.
pub struct Tab {
    uri: SurfaceUri,
    page: Page,
    /// Shared with the other tabs in screencast mode, owned in display mode.
    instance: Arc<Instance>,
    hub: Arc<FrameHub>,
    log: Arc<Mutex<InputLog>>,
    size: Arc<Mutex<(u32, u32)>>,
    /// Held while an input is dispatched so inputs reach Chrome in order.
    mapper: tokio::sync::Mutex<InputMapper>,
    label: Mutex<String>,
    feed: Feed,
}

/// Everything a tab is made of, put together by the server.
pub(crate) struct TabParts {
    pub uri: SurfaceUri,
    pub page: Page,
    pub instance: Arc<Instance>,
    pub hub: Arc<FrameHub>,
    pub log: Arc<Mutex<InputLog>>,
    pub size: Arc<Mutex<(u32, u32)>>,
    pub feed: Feed,
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
            feed: parts.feed,
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
        label.clone_into(&mut self.label.lock().unwrap_or_else(|e| e.into_inner()));
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
        *self.size.lock().unwrap_or_else(|e| e.into_inner())
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
        self.log.lock().unwrap_or_else(|e| e.into_inner()).record(event.seq, now_us());
        Ok(())
    }

    /// Sets the page viewport. The screencast restarts at the new size; the
    /// virtual display keeps its screen and the capture is clipped to it.
    async fn resize(&self, width: u32, height: u32) -> Result<()> {
        let (width, height) = match &self.feed {
            Feed::Display { screen, .. } => (width.min(screen.0), height.min(screen.1)),
            Feed::Screencast { .. } => (width, height),
        };
        if (width, height) == self.size() {
            return Ok(());
        }
        if let Feed::Screencast { .. } = self.feed {
            screencast::stop(&self.page).await?;
        }
        set_viewport(&self.page, width, height).await?;
        *self.size.lock().unwrap_or_else(|e| e.into_inner()) = (width, height);
        match &self.feed {
            Feed::Screencast { quality, .. } => screencast::start(&self.page, width, height, *quality).await?,
            Feed::Display { target, .. } => target.keyframe.store(true, Ordering::Relaxed),
        }
        Ok(())
    }

    /// Stops the frame feed. The page itself is closed by the server.
    pub(crate) fn stop_feed(&self) {
        match &self.feed {
            Feed::Screencast { pump, .. } => pump.abort(),
            Feed::Display { target, .. } => target.stop.store(true, Ordering::Relaxed),
        }
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
            name: self.label.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            width,
            height,
            mode: SurfaceMode::Pixel,
            mime: None,
        }
    }

    fn frames(&self) -> loa_surface::Result<broadcast::Receiver<Arc<Frame>>> {
        let receiver = self.hub.subscribe();
        // Navigation can finish after Chrome's only repaint of a static page.
        // A subscriber must still receive the current image without an input.
        if matches!(self.feed, Feed::Screencast { .. }) {
            self.hub.republish_keyframe();
        }
        Ok(receiver)
    }

    async fn request_keyframe(&self) -> loa_surface::Result<()> {
        match &self.feed {
            // Every screencast frame is a whole JPEG. A static page sends no
            // new ones, so the last is sent again.
            Feed::Screencast { .. } => {
                self.hub.republish_keyframe();
            }
            Feed::Display { target, .. } => target.keyframe.store(true, Ordering::Relaxed),
        }
        Ok(())
    }

    async fn input(&self, event: InputEvent) -> loa_surface::Result<()> {
        Ok(self.dispatch(&event).await?)
    }

    async fn document(&self) -> loa_surface::Result<Document> {
        Err(SurfaceError::Mode { uri: self.uri.clone(), mode: SurfaceMode::Pixel })
    }

    async fn screenshot(&self) -> loa_surface::Result<Bytes> {
        let params = CaptureScreenshotParams { format: Some(CaptureScreenshotFormat::Png), ..Default::default() };
        let shot = self.page.execute(params).await.map_err(BrowserError::from)?;
        let png = STANDARD.decode(shot.result.data.as_ref() as &str).map_err(|e| BrowserError::Script(e.to_string()))?;
        Ok(Bytes::from(png))
    }
}
