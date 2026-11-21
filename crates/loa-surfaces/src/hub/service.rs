//! [`SurfaceHub`]: a registry of [`SurfaceProvider`]s, the open surfaces and their events.

use crate::browser::{BrowserConfig, Tab};
use crate::docs::{DocSurface, Fonts};
use crate::hub::providers::{BrowserProvider, DocsProvider, McpDeclared, VncProvider};
use crate::hub::{HubError, Result};
use crate::vnc::RfbOptions;
use async_trait::async_trait;
use loa_contracts::surface::{Surface, SurfaceInfo, SurfaceUri};
use loa_mcp::Manager;
use loa_util::LockExt;
use loa_util::registry::Registry;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// What the hub starts with.
#[derive(Clone, Debug)]
pub struct HubConfig {
    /// The browser for browser surfaces; `None` when no Chrome or Chromium is installed.
    pub browser: Option<BrowserConfig>,
    /// Fonts for document pages and agent screenshots.
    pub fonts: Fonts,
    /// Where text-only `surface_screenshot` results are saved.
    pub screenshot_dir: PathBuf,
    pub rfb: RfbOptions,
    /// Live video for viewers across the network.
    #[cfg(feature = "remote-video")]
    pub video: crate::hub::video::VideoConfig,
    /// How browser tabs are captured.
    pub capture: CaptureMode,
    /// What the virtual display capture runs with, or why it can't run.
    pub virtual_display: Result<VirtualDisplaySetup, String>,
}

/// How browser tabs are captured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptureMode {
    /// Headless Chrome's JPEG screencast.
    #[default]
    Screencast,
    /// Headed Chrome on an Xvfb virtual display per tab, captured raw.
    VirtualDisplay,
}

/// Xvfb and a Chrome that can run headed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualDisplaySetup {
    pub xvfb: PathBuf,
    pub chrome: PathBuf,
}

impl VirtualDisplaySetup {
    /// Xvfb on `path_var` and a headed Chrome, or what is missing.
    pub fn find(path_var: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Result<Self, String> {
        let xvfb = crate::browser::find_xvfb(path_var).ok_or("Xvfb isn't installed (install the xvfb package)")?;
        let chrome = crate::browser::find_chrome(path_var, home, true)
            .ok_or("no Chrome or Chromium that runs headed (the headless shell can't)")?;
        Ok(Self { xvfb, chrome })
    }
}

impl HubConfig {
    /// Finds Chrome, Xvfb, ffmpeg and the system fonts; profiles and screenshots go under `state_dir`.
    pub fn discover(state_dir: &Path) -> Self {
        let browser = BrowserConfig::discover().ok().map(|mut config| {
            config.profile_root = Some(state_dir.join("browser"));
            config.sandbox = user_namespaces();
            config
        });
        let path = std::env::var_os("PATH");
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self {
            browser,
            fonts: Fonts::system(),
            screenshot_dir: state_dir.join("screenshots"),
            rfb: RfbOptions::default(),
            #[cfg(feature = "remote-video")]
            video: crate::hub::video::VideoConfig::discover(),
            capture: CaptureMode::Screencast,
            virtual_display: VirtualDisplaySetup::find(path.as_deref(), home.as_deref()),
        }
    }
}

/// Whether unprivileged user namespaces (Chrome's Linux sandbox) are allowed.
pub fn user_namespaces() -> bool {
    let flag = |path: &str| std::fs::read_to_string(path).ok().map(|s| s.trim() == "1");
    let restricted = flag("/proc/sys/kernel/apparmor_restrict_unprivileged_userns") == Some(true);
    let disabled = flag("/proc/sys/kernel/unprivileged_userns_clone") == Some(false);
    !(restricted || disabled)
}

/// Which provider a surface comes from.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    Browser,
    Document,
    Vm,
    /// Declared by the named third party MCP server.
    Mcp(String),
}

impl Origin {
    /// The server name shown and recorded with the surface.
    pub fn server(&self) -> &str {
        match self {
            Origin::Mcp(server) => server,
            other => other.provider(),
        }
    }

    /// The registry key of the provider.
    pub fn provider(&self) -> &'static str {
        match self {
            Origin::Browser => crate::browser::SERVER_NAME,
            Origin::Document => crate::docs::SERVER_NAME,
            Origin::Vm => crate::vnc::SERVER_NAME,
            Origin::Mcp(_) => "mcp",
        }
    }
}

/// An open surface; `surface` serves viewers and tools in this process.
#[derive(Clone)]
pub struct Opened {
    /// The surface as it was when opened.
    pub info: SurfaceInfo,
    pub origin: Origin,
    pub surface: Arc<dyn Surface>,
}

impl Opened {
    pub fn new(surface: Arc<dyn Surface>, origin: Origin) -> Self {
        Self { info: surface.info(), origin, surface }
    }
}

impl std::fmt::Debug for Opened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opened").field("info", &self.info).field("origin", &self.origin).finish_non_exhaustive()
    }
}

/// A surface the Surfaces tab lists: open, or ready to be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    pub info: SurfaceInfo,
    pub origin: Origin,
    pub open: bool,
}

/// What changed in the hub, for the UI and the session record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HubEvent {
    Opened { info: SurfaceInfo, origin: Origin },
    Closed { uri: SurfaceUri },
    /// The surfaces MCP servers declare changed.
    Declared,
}

/// What to open; each variant goes to one provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenRequest {
    /// A browser tab at a URL.
    Url(String),
    /// A document file.
    File(PathBuf),
    /// An RFB server at `host:port`; `password` answers VNC Authentication.
    Vm { address: String, password: Option<String> },
    /// A listed surface (a VM endpoint or a declared surface).
    Uri(SurfaceUri),
}

/// A source of surfaces the hub opens and closes.
#[async_trait]
pub trait SurfaceProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Surfaces that can be opened by uri while closed.
    async fn list(&self) -> Vec<Listed> {
        Vec::new()
    }
    async fn open(&self, req: &OpenRequest) -> Result<Opened>;
    async fn close(&self, _uri: &SurfaceUri) -> Result<()> {
        Ok(())
    }
    async fn shutdown(&self) {}
}

/// The surface service: the providers, the open surfaces and their events.
pub struct SurfaceHub {
    config: HubConfig,
    providers: Registry<dyn SurfaceProvider>,
    browser: Arc<BrowserProvider>,
    docs: Arc<DocsProvider>,
    mcp: Arc<McpDeclared>,
    open: Mutex<Vec<Opened>>,
    events: broadcast::Sender<HubEvent>,
    #[cfg(feature = "remote-video")]
    video: crate::hub::video::RemoteVideo,
}

impl SurfaceHub {
    pub fn new(config: HubConfig) -> Arc<Self> {
        let browser = Arc::new(BrowserProvider::new(config.browser.clone(), config.virtual_display.clone(), config.capture));
        let docs = Arc::new(DocsProvider::new(config.fonts.clone()));
        let mcp = Arc::new(McpDeclared::default());
        let mut providers: Registry<dyn SurfaceProvider> = Registry::new();
        let all: [Arc<dyn SurfaceProvider>; 4] = [browser.clone(), docs.clone(), mcp.clone(), Arc::new(VncProvider::new(config.rfb.clone()))];
        for provider in all {
            providers.register(provider.name(), provider);
        }
        Arc::new(Self {
            #[cfg(feature = "remote-video")]
            video: crate::hub::video::RemoteVideo::new(&config.video),
            config,
            providers,
            browser,
            docs,
            mcp,
            open: Mutex::new(Vec::new()),
            events: broadcast::channel(64).0,
        })
    }

    pub fn config(&self) -> &HubConfig {
        &self.config
    }

    pub fn browser_executable(&self) -> Option<&Path> {
        self.config.browser.as_ref().map(|b| b.executable.as_path())
    }

    pub fn browser_sandboxed(&self) -> bool {
        self.config.browser.as_ref().is_some_and(|b| b.sandbox)
    }

    pub fn events(&self) -> broadcast::Receiver<HubEvent> {
        self.events.subscribe()
    }

    fn emit(&self, event: HubEvent) {
        // No receiver is fine: nobody is watching yet.
        let _ = self.events.send(event);
    }

    /// Opens `req` with its provider; an already open surface is returned as is.
    pub async fn open_request(&self, req: OpenRequest) -> Result<Opened> {
        let provider = match &req {
            OpenRequest::Url(_) => crate::browser::SERVER_NAME,
            OpenRequest::File(_) => crate::docs::SERVER_NAME,
            OpenRequest::Vm { .. } => crate::vnc::SERVER_NAME,
            OpenRequest::Uri(uri) => {
                if let Some(opened) = self.opened(uri) {
                    return Ok(opened);
                }
                self.listed().await.into_iter().find(|l| &l.info.uri == uri).ok_or_else(|| HubError::NotFound(uri.clone()))?.origin.provider()
            }
        };
        let provider = self.providers.get(provider).ok_or_else(|| HubError::Unsupported(provider.to_owned()))?;
        let opened = provider.open(&req).await?;
        Ok(self.register(opened))
    }

    /// Opens a browser tab at `url` (see [`normalize_url`]), launching the browser on first use.
    pub async fn open_browser(&self, url: &str) -> Result<Opened> {
        self.open_request(OpenRequest::Url(url.to_owned())).await
    }

    /// Opens a PDF, XLSX, DOCX, PPTX or image file as a document surface.
    pub async fn open_document(&self, path: &Path) -> Result<Opened> {
        self.open_request(OpenRequest::File(path.to_owned())).await
    }

    /// Connects to the RFB server at `host:port` as `surface://vm/<host:port>`.
    pub async fn open_vm(&self, address: &str, password: Option<String>) -> Result<Opened> {
        self.open_request(OpenRequest::Vm { address: address.to_owned(), password }).await
    }

    /// Opens any listed surface.
    pub async fn open(&self, uri: &SurfaceUri) -> Result<Opened> {
        self.open_request(OpenRequest::Uri(uri.clone())).await
    }

    #[cfg(feature = "remote-video")]
    pub fn video(&self) -> &crate::hub::video::RemoteVideo {
        &self.video
    }

    /// The open surface `uri` for a viewer across the network.
    #[cfg(feature = "remote-video")]
    pub async fn network_surface(&self, uri: &SurfaceUri) -> Result<Arc<dyn Surface>> {
        let opened = self.opened(uri).ok_or_else(|| HubError::NotFound(uri.clone()))?;
        self.video.surface(opened).await
    }

    pub fn capture(&self) -> CaptureMode {
        self.browser.capture()
    }

    pub fn set_capture(&self, mode: CaptureMode) -> Result<()> {
        self.browser.set_capture(mode)
    }

    pub async fn capture_pending(&self) -> bool {
        self.browser.capture_pending().await
    }

    pub async fn tab(&self, uri: &SurfaceUri) -> Result<Arc<Tab>> {
        self.browser.tab(uri).await
    }

    pub fn document(&self, uri: &SurfaceUri) -> Result<DocSurface> {
        self.docs.document(uri)
    }

    fn register(&self, opened: Opened) -> Opened {
        {
            let mut open = self.open.locked();
            if let Some(existing) = open.iter().find(|o| o.info.uri == opened.info.uri) {
                return existing.clone();
            }
            open.push(opened.clone());
        }
        self.emit(HubEvent::Opened { info: opened.info.clone(), origin: opened.origin.clone() });
        opened
    }

    pub fn opened(&self, uri: &SurfaceUri) -> Option<Opened> {
        self.open.locked().iter().find(|o| &o.info.uri == uri).cloned()
    }

    /// Open surfaces in the order they were opened.
    pub fn open_surfaces(&self) -> Vec<Opened> {
        self.open.locked().clone()
    }

    /// Closes `uri`; VM endpoints and declared surfaces stay listed.
    pub async fn close(&self, uri: &SurfaceUri) -> Result<()> {
        let opened = {
            let mut open = self.open.locked();
            let index = open.iter().position(|o| &o.info.uri == uri).ok_or_else(|| HubError::NotFound(uri.clone()))?;
            open.remove(index)
        };
        #[cfg(feature = "remote-video")]
        self.video.forget(Some(uri));
        if let Some(provider) = self.providers.get(opened.origin.provider()) {
            provider.close(uri).await?;
        }
        self.emit(HubEvent::Closed { uri: uri.clone() });
        Ok(())
    }

    async fn listed(&self) -> Vec<Listed> {
        let mut listed = Vec::new();
        for (_, provider) in self.providers.iter() {
            listed.extend(provider.list().await);
        }
        listed
    }

    /// Open surfaces first, then the closed ones providers list.
    pub async fn list(&self) -> Vec<Listed> {
        let open = self.open_surfaces();
        let mut listed: Vec<Listed> =
            open.iter().map(|o| Listed { info: o.surface.info(), origin: o.origin.clone(), open: true }).collect();
        let closed = self.listed().await.into_iter().filter(|l| !open.iter().any(|o| o.info.uri == l.info.uri));
        listed.extend(closed);
        listed
    }

    /// Surfaces MCP servers declare, by server.
    pub fn declared(&self) -> BTreeMap<String, Vec<SurfaceInfo>> {
        self.mcp.declared()
    }

    /// Reads the `surface://` resources of every running server of `manager`; returns the failed listings.
    pub async fn refresh_declared(&self, manager: &Manager) -> Vec<(String, String)> {
        let failed = self.mcp.refresh(manager).await;
        self.emit(HubEvent::Declared);
        failed
    }

    /// Closes every surface and stops the providers.
    pub async fn shutdown(&self) {
        self.open.locked().clear();
        #[cfg(feature = "remote-video")]
        self.video.forget(None);
        for (_, provider) in self.providers.iter() {
            provider.shutdown().await;
        }
    }
}

/// Adds `https://` (or `http://` for localhost) to an address typed without a scheme.
pub fn normalize_url(url: &str) -> String {
    let url = url.trim();
    let has_scheme = url.contains("://") || ["about:", "data:", "file:", "chrome:"].iter().any(|s| url.starts_with(s));
    if has_scheme || url.is_empty() {
        url.to_owned()
    } else if url.starts_with("localhost") || url.starts_with("127.") {
        format!("http://{url}")
    } else {
        format!("https://{url}")
    }
}
