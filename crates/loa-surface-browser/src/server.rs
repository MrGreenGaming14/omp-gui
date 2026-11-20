//! The `browser` surface server: launches Chrome and keeps its tabs.

use crate::chrome::{BrowserConfig, Capture, VirtualDisplayConfig};
use crate::frames::{FrameHub, InputLog};
use crate::tab::{Feed, Tab, TabParts, set_viewport};
#[cfg(target_os = "linux")]
use crate::frames::CaptureTarget;
#[cfg(target_os = "linux")]
use crate::x11::{X11Capture, spawn_capture};
#[cfg(target_os = "linux")]
use crate::xvfb::VirtualDisplay;
use crate::{BrowserError, Result, SERVER_NAME, screencast};
use async_trait::async_trait;
use chromiumoxide::browser::{Browser, BrowserConfig as CdpConfig};
use chromiumoxide::cdp::browser_protocol::page::EventScreencastFrame;
use chromiumoxide::Page;
use futures::StreamExt;
use loa_surface::{Surface, SurfaceInfo, SurfaceServer, SurfaceUri};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::task::JoinHandle;

/// Xvfb exists only on Linux; no instance of this is ever made elsewhere.
#[cfg(not(target_os = "linux"))]
enum VirtualDisplay {}

#[cfg(not(target_os = "linux"))]
impl VirtualDisplay {
    fn display(&self) -> String {
        match *self {}
    }

    async fn stop(self) -> Result<()> {
        match self {}
    }
}

/// How long Chrome gets to exit after `Browser.close` before it is killed.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// One running Chrome process and its DevTools connection.
pub(crate) struct Instance {
    browser: tokio::sync::Mutex<Browser>,
    handler: JoinHandle<()>,
    /// The Xvfb server the browser draws on, in display mode.
    display: Mutex<Option<VirtualDisplay>>,
    _profile: TempDir,
}

/// chromiumoxide's default flags without `enable-automation`, plus the
/// switches that keep infobars off a headed window.
const HEADED_ARGS: [&str; 26] = [
    "disable-background-networking",
    "enable-features=NetworkService,NetworkServiceInProcess",
    "disable-background-timer-throttling",
    "disable-backgrounding-occluded-windows",
    "disable-breakpad",
    "disable-client-side-phishing-detection",
    "disable-component-extensions-with-background-pages",
    "disable-default-apps",
    "disable-dev-shm-usage",
    "disable-features=TranslateUI",
    "disable-hang-monitor",
    "disable-ipc-flooding-protection",
    "disable-popup-blocking",
    "disable-prompt-on-repost",
    "disable-renderer-backgrounding",
    "disable-sync",
    "force-color-profile=srgb",
    "metrics-recording-only",
    "no-first-run",
    "password-store=basic",
    "use-mock-keychain",
    "enable-blink-features=IdleDetection",
    "lang=en_US",
    "disable-infobars",
    // Hides the "unsupported command-line flag" bar `--no-sandbox` brings.
    "test-type",
    "noerrdialogs",
];

impl Instance {
    async fn launch(config: &BrowserConfig, display: Option<VirtualDisplay>) -> Result<Arc<Self>> {
        let profile = match &config.profile_root {
            Some(root) => tempfile::Builder::new().prefix("loa-chrome-").tempdir_in(root)?,
            None => tempfile::Builder::new().prefix("loa-chrome-").tempdir()?,
        };
        let mut builder = CdpConfig::builder()
            .chrome_executable(&config.executable)
            .user_data_dir(profile.path())
            .viewport(None)
            .window_size(config.width, config.height)
            .args(config.extra_args.iter().map(|a| a.trim_start_matches("--").to_owned()));
        if !config.sandbox {
            builder = builder.no_sandbox();
        }
        builder = match &display {
            // The builder defaults to headless.
            None => builder,
            // Headed: chromiumoxide's default flags minus `enable-automation`,
            // whose infobar (and Chrome for Testing's) would sit on top of the
            // page in the captured picture, shifting it away from where
            // input lands.
            Some(display) => builder
                .with_head()
                .disable_default_args()
                .env("DISPLAY", display.display())
                .args(HEADED_ARGS)
                .args(["ozone-platform=x11", "kiosk", "window-position=0,0", "no-default-browser-check"]),
        };
        let (browser, mut handler) = Browser::launch(builder.build().map_err(BrowserError::Config)?).await?;
        let handler = tokio::spawn(async move {
            while let Some(event) = handler.next().await {
                if let Err(error) = event {
                    tracing::debug!(%error, "chrome devtools connection");
                }
            }
        });
        Ok(Arc::new(Self {
            browser: tokio::sync::Mutex::new(browser),
            handler,
            display: Mutex::new(display),
            _profile: profile,
        }))
    }

    async fn new_page(&self) -> Result<Page> {
        Ok(self.browser.lock().await.new_page("about:blank").await?)
    }

    async fn pid(&self) -> Option<u32> {
        self.browser.lock().await.get_mut_child().and_then(|c| c.as_mut_inner().id())
    }

    /// Closes Chrome (killing it if it does not exit in time), then the display.
    async fn shutdown(&self) -> Result<()> {
        {
            let mut browser = self.browser.lock().await;
            let exited = match tokio::time::timeout(CLOSE_TIMEOUT, browser.close()).await {
                Ok(Ok(_)) => matches!(tokio::time::timeout(CLOSE_TIMEOUT, browser.wait()).await, Ok(Ok(_))),
                _ => false,
            };
            if !exited && let Some(Err(error)) = browser.kill().await {
                tracing::warn!(%error, "killing chrome");
            }
        }
        self.handler.abort();
        let display = self.display.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(display) = display {
            display.stop().await?;
        }
        Ok(())
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // The Chrome and Xvfb processes are killed on drop by their handles.
        self.handler.abort();
    }
}

/// The `browser` surface server. Tabs are `surface://browser/tab<n>`.
pub struct BrowserServer {
    config: BrowserConfig,
    /// The one headless browser all tabs share, in screencast mode.
    shared: Option<Arc<Instance>>,
    tabs: Mutex<BTreeMap<u64, Arc<Tab>>>,
    next: AtomicU64,
    closed: AtomicBool,
}

impl BrowserServer {
    /// Starts the server. In screencast mode this launches headless Chrome
    /// now; in display mode each tab gets its own display and browser when
    /// it opens.
    pub async fn launch(config: BrowserConfig) -> Result<Self> {
        let shared = match config.capture {
            Capture::Screencast => Some(Instance::launch(&config, None).await?),
            Capture::VirtualDisplay(_) => None,
        };
        Ok(Self { config, shared, tabs: Mutex::default(), next: AtomicU64::new(0), closed: AtomicBool::new(false) })
    }

    pub fn config(&self) -> &BrowserConfig {
        &self.config
    }

    /// Opens a tab at the configured size and loads `url` in it.
    pub async fn open_tab(&self, url: &str) -> Result<Arc<Tab>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(BrowserError::Closed);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let uri = SurfaceUri::new(SERVER_NAME, &format!("tab{id}")).map_err(|e| BrowserError::Config(e.to_string()))?;
        let tab = Arc::new(match &self.config.capture {
            Capture::Screencast => self.open_screencast_tab(uri).await?,
            Capture::VirtualDisplay(display) => self.open_display_tab(uri, display).await?,
        });
        if let Err(error) = tab.navigate(url).await {
            self.close(&tab).await;
            return Err(error);
        }
        self.tabs.lock().unwrap_or_else(|e| e.into_inner()).insert(id, tab.clone());
        Ok(tab)
    }

    async fn open_screencast_tab(&self, uri: SurfaceUri) -> Result<Tab> {
        let instance = self.shared.clone().ok_or(BrowserError::Closed)?;
        let (width, height) = (self.config.width, self.config.height);
        let page = instance.new_page().await?;
        let started = async {
            set_viewport(&page, width, height).await?;
            let events = page.event_listener::<EventScreencastFrame>().await?;
            screencast::start(&page, width, height, self.config.jpeg_quality).await?;
            Ok::<_, BrowserError>(events)
        };
        let events = match started.await {
            Ok(events) => events,
            Err(error) => {
                let _ = page.close().await;
                return Err(error);
            }
        };
        let hub = Arc::new(FrameHub::new());
        let log = Arc::new(Mutex::new(InputLog::default()));
        let pump = tokio::spawn(screencast::pump(page.clone(), events, hub.clone(), log.clone()));
        Ok(Tab::new(TabParts {
            uri,
            page,
            instance,
            hub,
            log,
            size: Arc::new(Mutex::new((width, height))),
            feed: Feed::Screencast { pump, quality: self.config.jpeg_quality },
        }))
    }

    #[cfg(target_os = "linux")]
    async fn open_display_tab(&self, uri: SurfaceUri, config: &VirtualDisplayConfig) -> Result<Tab> {
        let (width, height) = (self.config.width, self.config.height);
        let display = VirtualDisplay::start(&config.xvfb, &config.socket_dir, width, height).await?;
        let socket = display.socket().to_owned();
        let instance = Instance::launch(&self.config, Some(display)).await?;
        let opened = async {
            let page = instance.new_page().await?;
            set_viewport(&page, width, height).await?;
            let capture = tokio::task::spawn_blocking(move || X11Capture::connect(&socket))
                .await
                .map_err(|e| BrowserError::X11(e.to_string()))??;
            Ok::<_, BrowserError>((page, capture))
        };
        let (page, capture) = match opened.await {
            Ok(opened) => opened,
            Err(error) => {
                let _ = instance.shutdown().await;
                return Err(error);
            }
        };
        let hub = Arc::new(FrameHub::new());
        let log = Arc::new(Mutex::new(InputLog::default()));
        let size = Arc::new(Mutex::new((width, height)));
        let screen = capture.screen_size();
        let target = Arc::new(CaptureTarget {
            hub: hub.clone(),
            log: log.clone(),
            size: size.clone(),
            stop: AtomicBool::new(false),
            keyframe: AtomicBool::new(true),
            interval: Duration::from_secs(1) / config.fps.max(1),
        });
        // The thread ends when the tab sets `stop` or the display goes away.
        drop(spawn_capture(capture, target.clone()));
        Ok(Tab::new(TabParts { uri, page, instance, hub, log, size, feed: Feed::Display { target, screen } }))
    }

    #[cfg(not(target_os = "linux"))]
    async fn open_display_tab(&self, _uri: SurfaceUri, _config: &VirtualDisplayConfig) -> Result<Tab> {
        Err(BrowserError::Display(crate::chrome::VIRTUAL_DISPLAY_UNAVAILABLE.into()))
    }

    /// The open tab behind `uri`.
    pub fn tab(&self, uri: &SurfaceUri) -> Result<Arc<Tab>> {
        let tabs = self.tabs.lock().unwrap_or_else(|e| e.into_inner());
        tabs.values().find(|t| t.uri() == uri).cloned().ok_or_else(|| BrowserError::NotFound(uri.clone()))
    }

    /// Open tabs in the order they were opened.
    pub fn tabs(&self) -> Vec<Arc<Tab>> {
        self.tabs.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }

    /// Closes the tab behind `uri`. Its viewers' frame streams end.
    pub async fn close_tab(&self, uri: &SurfaceUri) -> Result<()> {
        let tab = {
            let mut tabs = self.tabs.lock().unwrap_or_else(|e| e.into_inner());
            let id = tabs.iter().find(|(_, t)| t.uri() == uri).map(|(id, _)| *id);
            id.and_then(|id| tabs.remove(&id)).ok_or_else(|| BrowserError::NotFound(uri.clone()))?
        };
        self.close(&tab).await;
        Ok(())
    }

    async fn close(&self, tab: &Tab) {
        tab.stop_feed();
        let result = if self.shared.as_ref().is_some_and(|shared| Arc::ptr_eq(shared, tab.instance())) {
            tab.page().clone().close().await.map_err(BrowserError::from)
        } else {
            tab.instance().shutdown().await
        };
        if let Err(error) = result {
            tracing::warn!(%error, uri = %tab.uri(), "closing tab");
        }
    }

    /// Process ids of the running browsers.
    pub async fn process_ids(&self) -> Vec<u32> {
        let mut ids = Vec::new();
        if let Some(shared) = &self.shared {
            ids.extend(shared.pid().await);
        }
        for tab in self.tabs() {
            if !self.shared.as_ref().is_some_and(|shared| Arc::ptr_eq(shared, tab.instance())) {
                ids.extend(tab.instance().pid().await);
            }
        }
        ids
    }

    /// Closes every tab and stops every browser and display. Later calls to
    /// [`open_tab`](Self::open_tab) fail with [`BrowserError::Closed`].
    pub async fn shutdown(&self) -> Result<()> {
        self.closed.store(true, Ordering::Release);
        let tabs = std::mem::take(&mut *self.tabs.lock().unwrap_or_else(|e| e.into_inner()));
        for tab in tabs.values() {
            tab.stop_feed();
            if !self.shared.as_ref().is_some_and(|shared| Arc::ptr_eq(shared, tab.instance())) {
                tab.instance().shutdown().await?;
            }
        }
        if let Some(shared) = &self.shared {
            shared.shutdown().await?;
        }
        Ok(())
    }
}

#[async_trait]
impl SurfaceServer for BrowserServer {
    fn name(&self) -> &str {
        SERVER_NAME
    }

    async fn list(&self) -> loa_surface::Result<Vec<SurfaceInfo>> {
        Ok(self.tabs().iter().map(|t| t.info()).collect())
    }

    async fn open(&self, uri: &SurfaceUri) -> loa_surface::Result<Arc<dyn Surface>> {
        if uri.server() != SERVER_NAME {
            return Err(loa_surface::SurfaceError::NotFound(uri.clone()));
        }
        Ok(self.tab(uri)?)
    }
}
