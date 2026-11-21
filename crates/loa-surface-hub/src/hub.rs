//! [`SurfaceHub`]: the surfaces of this app, whoever opened them.

use loa_util::LockExt;
use crate::mcp_surface::McpSurface;
use crate::{HubError, Result};
use async_trait::async_trait;
use loa_mcp::{AllowAll, Manager, Status};
use loa_contracts::surface::{Surface, SurfaceInfo, SurfaceMode, SurfaceServer, SurfaceUri};
use loa_surface_browser::{BrowserConfig, BrowserServer, Capture, Tab, VirtualDisplayConfig};
use loa_surface_docs::{DocSurface, DocsServer, Fonts};
use loa_surface_rfb::{PasswordSource, RfbEndpoint, RfbOptions, RfbServer};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// What the hub starts with.
#[derive(Clone, Debug)]
pub struct HubConfig {
    /// The browser to launch on the first browser surface. `None` when no
    /// Chrome or Chromium is installed; browser surfaces then fail with
    /// [`HubError::NoBrowser`].
    pub browser: Option<BrowserConfig>,
    /// Fonts for rendering document pages and agent screenshots.
    pub fonts: Fonts,
    /// Where `surface_screenshot` host tool results are saved for harnesses
    /// that only take text results.
    pub screenshot_dir: PathBuf,
    pub rfb: RfbOptions,
    /// Live video for viewers across the network.
    #[cfg(feature = "remote-video")]
    pub video: crate::video::VideoConfig,
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
    /// Xvfb on `path_var` and a headed Chrome (search path, install
    /// locations, the Playwright cache under `home`), or what is missing.
    pub fn find(path_var: Option<&std::ffi::OsStr>, home: Option<&Path>) -> Result<Self, String> {
        let xvfb = loa_surface_browser::find_xvfb(path_var)
            .ok_or_else(|| "Xvfb isn't installed (install the xvfb package)".to_owned())?;
        let chrome = loa_surface_browser::find_chrome(path_var, home, true)
            .ok_or_else(|| "no Chrome or Chromium that runs headed (the headless shell can't)".to_owned())?;
        Ok(Self { xvfb, chrome })
    }
}

impl HubConfig {
    /// Finds Chrome or Chromium (search path, install locations, then the
    /// Playwright cache), Xvfb, ffmpeg and the system fonts. Browser
    /// profiles and agent screenshots go under `state_dir`. Chrome's sandbox
    /// is on unless the kernel refuses unprivileged user namespaces (see
    /// [`user_namespaces`]), where Chrome would refuse to start with it.
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
            video: crate::video::VideoConfig::discover(),
            capture: CaptureMode::Screencast,
            virtual_display: VirtualDisplaySetup::find(path.as_deref(), home.as_deref()),
        }
    }
}

/// Whether unprivileged processes may create user namespaces, which
/// Chrome's Linux sandbox needs (unless Chrome ships a setuid helper, which
/// the Playwright builds don't). Ubuntu 23.10+ restricts them with AppArmor;
/// older kernels may turn them off with `unprivileged_userns_clone`.
pub fn user_namespaces() -> bool {
    let flag = |path: &str| std::fs::read_to_string(path).ok().map(|s| s.trim() == "1");
    let restricted = flag("/proc/sys/kernel/apparmor_restrict_unprivileged_userns") == Some(true);
    let disabled = flag("/proc/sys/kernel/unprivileged_userns_clone") == Some(false);
    !(restricted || disabled)
}

/// Which server a surface comes from.
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
            Origin::Browser => loa_surface_browser::SERVER_NAME,
            Origin::Document => loa_surface_docs::SERVER_NAME,
            Origin::Vm => loa_surface_rfb::SERVER_NAME,
            Origin::Mcp(server) => server,
        }
    }
}

/// An open surface.
#[derive(Clone)]
pub struct Opened {
    /// The surface as it was when opened.
    pub info: SurfaceInfo,
    pub origin: Origin,
    /// The surface itself, for viewers and tools in this process: its
    /// frames come as the source makes them (raw RGBA, JPEG screencast,
    /// VNC rectangles), with no encoder in the way.
    pub surface: Arc<dyn Surface>,
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
    /// A surface opened, by the user or by an agent tool call.
    Opened { info: SurfaceInfo, origin: Origin },
    Closed { uri: SurfaceUri },
    /// The surfaces MCP servers declare changed.
    Declared,
}

/// VNC passwords the user typed, kept in memory for reconnects.
#[derive(Default)]
struct Passwords(Mutex<HashMap<String, String>>);

#[async_trait]
impl PasswordSource for Passwords {
    async fn password(&self, endpoint: &str) -> Option<String> {
        self.0.locked().get(endpoint).cloned()
    }
}

/// The surface service: built-in servers, open surfaces and the surfaces
/// MCP servers declare.
pub struct SurfaceHub {
    config: HubConfig,
    browser: tokio::sync::Mutex<Option<Arc<BrowserServer>>>,
    docs: DocsServer,
    vm: RfbServer,
    passwords: Arc<Passwords>,
    open: Mutex<Vec<Opened>>,
    declared: Mutex<BTreeMap<String, Vec<SurfaceInfo>>>,
    mcp: Mutex<Option<Manager>>,
    events: broadcast::Sender<HubEvent>,
    #[cfg(feature = "remote-video")]
    video: crate::video::RemoteVideo,
    capture: Mutex<CaptureMode>,
}

impl SurfaceHub {
    pub fn new(config: HubConfig) -> Arc<Self> {
        let passwords = Arc::new(Passwords::default());
        Arc::new(Self {
            docs: DocsServer::new(config.fonts.clone()),
            vm: RfbServer::new(Vec::new(), passwords.clone(), config.rfb.clone()),
            passwords,
            #[cfg(feature = "remote-video")]
            video: crate::video::RemoteVideo::new(&config.video),
            capture: Mutex::new(config.capture),
            config,
            browser: tokio::sync::Mutex::new(None),
            open: Mutex::new(Vec::new()),
            declared: Mutex::new(BTreeMap::new()),
            mcp: Mutex::new(None),
            events: broadcast::channel(64).0,
        })
    }

    pub fn config(&self) -> &HubConfig {
        &self.config
    }

    /// The browser executable browser surfaces run on, `None` when none is
    /// installed.
    pub fn browser_executable(&self) -> Option<&Path> {
        self.config.browser.as_ref().map(|b| b.executable.as_path())
    }

    /// Whether the browser runs with Chrome's sandbox.
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

    /// Opens a browser tab at `url` (see [`normalize_url`]), launching the
    /// browser on first use.
    pub async fn open_browser(&self, url: &str) -> Result<Opened> {
        let server = self.browser_server().await?;
        let tab = server.open_tab(&normalize_url(url)).await?;
        Ok(self.register(tab, Origin::Browser))
    }

    /// Live video for viewers across the network.
    #[cfg(feature = "remote-video")]
    pub fn video(&self) -> &crate::video::RemoteVideo {
        &self.video
    }

    /// The open surface `uri` for a viewer across the network (a peer over
    /// `omp/surface/1`); viewers here use [`Opened::surface`].
    #[cfg(feature = "remote-video")]
    pub async fn network_surface(&self, uri: &SurfaceUri) -> Result<Arc<dyn Surface>> {
        let opened = self.opened(uri).ok_or_else(|| HubError::NotFound(uri.clone()))?;
        self.video.surface(opened).await
    }

    /// How new browser tabs are captured.
    pub fn capture(&self) -> CaptureMode {
        *self.capture.locked()
    }

    /// Picks how new browser tabs are captured. The virtual display needs
    /// what [`HubConfig::virtual_display`] found.
    pub fn set_capture(&self, mode: CaptureMode) -> Result<()> {
        if mode == CaptureMode::VirtualDisplay
            && let Err(missing) = &self.config.virtual_display
        {
            return Err(HubError::NoVirtualDisplay(missing.clone()));
        }
        *self.capture.locked() = mode;
        Ok(())
    }

    /// Whether the running browser still captures the other way: it keeps
    /// doing so until its tabs are closed.
    pub async fn capture_pending(&self) -> bool {
        let browser = self.browser.lock().await.clone();
        browser.is_some_and(|server| capture_mode(&server) != self.capture() && !server.tabs().is_empty())
    }

    /// The browser config for `mode`.
    fn browser_config(&self, mode: CaptureMode) -> Result<BrowserConfig> {
        let mut config = self.config.browser.clone().ok_or(HubError::NoBrowser)?;
        if mode == CaptureMode::VirtualDisplay {
            let setup = self.config.virtual_display.clone().map_err(HubError::NoVirtualDisplay)?;
            config.executable = setup.chrome;
            config.capture = Capture::VirtualDisplay(VirtualDisplayConfig::new(setup.xvfb));
        }
        Ok(config)
    }

    /// The running browser, launched (or relaunched for another capture
    /// once its tabs are closed) as needed.
    async fn browser_server(&self) -> Result<Arc<BrowserServer>> {
        let mut browser = self.browser.lock().await;
        let mode = self.capture();
        if let Some(server) = &*browser {
            if capture_mode(server) == mode || !server.tabs().is_empty() {
                return Ok(server.clone());
            }
            if let Err(error) = server.shutdown().await {
                tracing::warn!(%error, "stopping the browser to switch its capture");
            }
            *browser = None;
        }
        let config = self.browser_config(mode)?;
        if let Some(root) = &config.profile_root {
            std::fs::create_dir_all(root).map_err(loa_surface_browser::BrowserError::from)?;
        }
        let server = Arc::new(BrowserServer::launch(config).await?);
        *browser = Some(server.clone());
        Ok(server)
    }

    /// The browser tab behind `uri`, for the browser's structured tools.
    pub async fn tab(&self, uri: &SurfaceUri) -> Result<Arc<Tab>> {
        let browser = self.browser.lock().await.clone();
        let server = browser.ok_or_else(|| HubError::NotFound(uri.clone()))?;
        server.tab(uri).map_err(|_| HubError::NotFound(uri.clone()))
    }

    /// Opens a PDF, XLSX, DOCX, PPTX or image file as a document surface.
    pub async fn open_document(&self, path: &Path) -> Result<Opened> {
        let surface = self.docs.open_file(path).await?;
        Ok(self.register(Arc::new(surface), Origin::Document))
    }

    /// The document surface behind `uri`, for the document data tool.
    pub fn document(&self, uri: &SurfaceUri) -> Result<DocSurface> {
        self.docs.surface(uri).map_err(|_| HubError::NotFound(uri.clone()))
    }

    /// Connects to the RFB (VNC) server at `address` (`host:port`) as
    /// `surface://vm/<host:port>`. `password` answers VNC Authentication.
    pub async fn open_vm(&self, address: &str, password: Option<String>) -> Result<Opened> {
        let address = address.trim();
        let valid = address
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && !host.contains('/') && port.parse::<u16>().is_ok());
        if !valid {
            return Err(HubError::Address(address.to_owned()));
        }
        {
            let mut passwords = self.passwords.0.locked();
            match password.filter(|p| !p.is_empty()) {
                Some(password) => passwords.insert(address.to_owned(), password),
                None => passwords.remove(address),
            };
        }
        let uri = self.vm.add_endpoint(RfbEndpoint { name: address.to_owned(), address: address.to_owned() })?;
        if let Some(opened) = self.opened(&uri) {
            return Ok(opened);
        }
        let surface = self.vm.open_rfb(&uri).await?;
        Ok(self.register(surface, Origin::Vm))
    }

    /// Opens any listed surface: an already open one, a configured VM
    /// endpoint, or one an MCP server declares.
    pub async fn open(&self, uri: &SurfaceUri) -> Result<Opened> {
        if let Some(opened) = self.opened(uri) {
            return Ok(opened);
        }
        let own_vm = uri.server() == loa_surface_rfb::SERVER_NAME
            && self.vm.list().await.unwrap_or_default().iter().any(|info| &info.uri == uri);
        if own_vm {
            let surface = self.vm.open_rfb(uri).await?;
            return Ok(self.register(surface, Origin::Vm));
        }
        let declared = self.declared.locked()
            .iter()
            .find_map(|(server, infos)| infos.iter().find(|i| &i.uri == uri).map(|i| (server.clone(), i.clone())));
        let (server, info) = declared.ok_or_else(|| HubError::NotFound(uri.clone()))?;
        let manager = self.mcp.locked().clone().ok_or(HubError::NoMcp)?;
        let surface = McpSurface::new(manager, server.clone(), info);
        Ok(self.register(Arc::new(surface), Origin::Mcp(server)))
    }

    fn register(&self, surface: Arc<dyn Surface>, origin: Origin) -> Opened {
        let opened = Opened { info: surface.info(), origin, surface };
        {
            let mut open = self.open.locked();
            open.retain(|o| o.info.uri != opened.info.uri);
            open.push(opened.clone());
        }
        self.emit(HubEvent::Opened { info: opened.info.clone(), origin: opened.origin.clone() });
        opened
    }

    /// The open surface behind `uri`.
    pub fn opened(&self, uri: &SurfaceUri) -> Option<Opened> {
        self.open.locked().iter().find(|o| &o.info.uri == uri).cloned()
    }

    /// Open surfaces in the order they were opened.
    pub fn open_surfaces(&self) -> Vec<Opened> {
        self.open.locked().clone()
    }

    /// Closes `uri`: the browser tab closes, the document is dropped, the
    /// VM connection ends once no viewer holds it. VM endpoints and declared
    /// surfaces stay listed for reopening.
    pub async fn close(&self, uri: &SurfaceUri) -> Result<()> {
        let opened = {
            let mut open = self.open.locked();
            let index = open.iter().position(|o| &o.info.uri == uri).ok_or_else(|| HubError::NotFound(uri.clone()))?;
            open.remove(index)
        };
        #[cfg(feature = "remote-video")]
        self.video.forget(Some(uri));
        match opened.origin {
            Origin::Browser => {
                let browser = self.browser.lock().await.clone();
                if let Some(server) = browser {
                    server.close_tab(uri).await?;
                }
            }
            Origin::Document => {
                self.docs.close(uri);
            }
            Origin::Vm | Origin::Mcp(_) => {}
        }
        self.emit(HubEvent::Closed { uri: uri.clone() });
        Ok(())
    }

    /// Everything the Surfaces tab lists: open surfaces first, then VM
    /// endpoints and declared surfaces that aren't open.
    pub async fn list(&self) -> Vec<Listed> {
        let open = self.open_surfaces();
        let is_open = |uri: &SurfaceUri| open.iter().any(|o| &o.info.uri == uri);
        let mut listed: Vec<Listed> = open
            .iter()
            .map(|o| Listed { info: o.surface.info(), origin: o.origin.clone(), open: true })
            .collect();
        let endpoints = self.vm.list().await.unwrap_or_default();
        listed.extend(
            endpoints
                .into_iter()
                .filter(|info| !is_open(&info.uri))
                .map(|info| Listed { info, origin: Origin::Vm, open: false }),
        );
        for (server, infos) in self.declared.locked().iter() {
            listed.extend(infos.iter().filter(|info| !is_open(&info.uri)).map(|info| Listed {
                info: info.clone(),
                origin: Origin::Mcp(server.clone()),
                open: false,
            }));
        }
        listed
    }

    /// Surfaces MCP servers declare, by server.
    pub fn declared(&self) -> BTreeMap<String, Vec<SurfaceInfo>> {
        self.declared.locked().clone()
    }

    /// Sets the surfaces `server` declares; an empty list forgets the server.
    pub fn set_declared(&self, server: &str, surfaces: Vec<SurfaceInfo>) {
        {
            let mut declared = self.declared.locked();
            if surfaces.is_empty() {
                declared.remove(server);
            } else {
                declared.insert(server.to_owned(), surfaces);
            }
        }
        self.emit(HubEvent::Declared);
    }

    /// Reads the `surface://` resources of every running MCP server in
    /// `manager` and keeps `manager` to open them with. Servers that aren't
    /// running are forgotten. Returns the servers whose listing failed.
    pub async fn refresh_declared(&self, manager: &Manager) -> Vec<(String, String)> {
        *self.mcp.locked() = Some(manager.clone());
        let mut declared = BTreeMap::new();
        let mut failed = Vec::new();
        for view in manager.view(&AllowAll) {
            if view.status != Status::Running {
                continue;
            }
            let name = view.spec.name;
            match manager.surfaces(&name).await {
                Ok(surfaces) => {
                    let infos: Vec<SurfaceInfo> = surfaces.into_iter().map(|(uri, resource)| declared_info(uri, &resource)).collect();
                    if !infos.is_empty() {
                        declared.insert(name, infos);
                    }
                }
                Err(error) => failed.push((name, error.to_string())),
            }
        }
        *self.declared.locked() = declared;
        self.emit(HubEvent::Declared);
        failed
    }

    /// Closes every surface and stops the browser.
    pub async fn shutdown(&self) {
        self.open.locked().clear();
        #[cfg(feature = "remote-video")]
        self.video.forget(None);
        let browser = self.browser.lock().await.take();
        if let Some(server) = browser
            && let Err(error) = server.shutdown().await
        {
            tracing::warn!(%error, "stopping the browser");
        }
    }
}

fn capture_mode(server: &BrowserServer) -> CaptureMode {
    match server.config().capture {
        Capture::Screencast => CaptureMode::Screencast,
        Capture::VirtualDisplay(_) => CaptureMode::VirtualDisplay,
    }
}

/// What a declared surface resource says about itself. Resources without the
/// `omp/surface` metadata are taken as pixel surfaces (documents when they
/// carry a MIME type) of unknown size.
fn declared_info(uri: SurfaceUri, resource: &loa_mcp::model::Resource) -> SurfaceInfo {
    let parsed = serde_json::to_value(resource)
        .ok()
        .and_then(|value| crate::mcp::parse_resource(&value).ok().flatten());
    parsed.unwrap_or_else(|| SurfaceInfo {
        uri,
        name: resource.title.clone().unwrap_or_else(|| resource.name.clone()),
        width: 0,
        height: 0,
        mode: if resource.mime_type.is_some() { SurfaceMode::Document } else { SurfaceMode::Pixel },
        mime: resource.mime_type.clone(),
    })
}

/// Adds `https://` to an address typed without a scheme, e.g. `example.com`.
/// `about:`, `data:`, `file:` and URLs with a scheme are kept.
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_addresses_get_a_scheme() {
        assert_eq!(normalize_url(" example.com "), "https://example.com");
        assert_eq!(normalize_url("localhost:8080/a"), "http://localhost:8080/a");
        assert_eq!(normalize_url("http://x.test"), "http://x.test");
        assert_eq!(normalize_url("about:blank"), "about:blank");
        assert_eq!(normalize_url("data:text/html,hi"), "data:text/html,hi");
    }
}
