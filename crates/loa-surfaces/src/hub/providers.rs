//! The [`SurfaceProvider`] adapters: the browser, documents, VNC endpoints and MCP declared surfaces.

use crate::browser::{BrowserConfig, BrowserServer, CaptureConfig, Tab, VirtualDisplayConfig};
use crate::docs::{DocSurface, DocsServer, Fonts};
use crate::hub::mcp_surface::McpSurface;
use crate::hub::service::{CaptureMode, Listed, OpenRequest, Opened, Origin, SurfaceProvider, VirtualDisplaySetup};
use crate::hub::{HubError, Result};
use crate::vnc::{PasswordSource, RfbEndpoint, RfbOptions, RfbServer};
use async_trait::async_trait;
use loa_contracts::surface::{SurfaceInfo, SurfaceMode, SurfaceServer, SurfaceUri};
use loa_mcp::{AllowAll, Manager, Status};
use loa_util::LockExt;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

fn unsupported(provider: &str, req: &OpenRequest) -> HubError {
    HubError::Unsupported(format!("the {provider} provider can't open {req:?}"))
}

/// Browser tabs on the installed Chrome, launched on first use and relaunched to switch capture.
pub struct BrowserProvider {
    config: Option<BrowserConfig>,
    virtual_display: Result<VirtualDisplaySetup, String>,
    capture: Mutex<CaptureMode>,
    server: tokio::sync::Mutex<Option<Arc<BrowserServer>>>,
}

impl BrowserProvider {
    pub(crate) fn new(config: Option<BrowserConfig>, virtual_display: Result<VirtualDisplaySetup, String>, capture: CaptureMode) -> Self {
        Self { config, virtual_display, capture: Mutex::new(capture), server: tokio::sync::Mutex::new(None) }
    }

    fn display(&self) -> Result<VirtualDisplaySetup> {
        self.virtual_display.clone().map_err(HubError::NoVirtualDisplay)
    }

    pub fn capture(&self) -> CaptureMode {
        *self.capture.locked()
    }

    /// Picks how new tabs are captured; the virtual display needs Xvfb and a headed Chrome.
    pub fn set_capture(&self, mode: CaptureMode) -> Result<()> {
        if mode == CaptureMode::VirtualDisplay {
            self.display()?;
        }
        *self.capture.locked() = mode;
        Ok(())
    }

    /// Whether the running browser still captures the other way (until its tabs are closed).
    pub async fn capture_pending(&self) -> bool {
        let server = self.server.lock().await.clone();
        server.is_some_and(|s| mode_of(&s) != self.capture() && !s.tabs().is_empty())
    }

    /// The running browser, launched (or relaunched for another capture once its tabs are closed) as needed.
    async fn server(&self) -> Result<Arc<BrowserServer>> {
        let mut slot = self.server.lock().await;
        let mode = self.capture();
        if let Some(server) = &*slot {
            if mode_of(server) == mode || !server.tabs().is_empty() {
                return Ok(server.clone());
            }
            if let Err(error) = server.shutdown().await {
                tracing::warn!(%error, "stopping the browser to switch its capture");
            }
            *slot = None;
        }
        let mut config = self.config.clone().ok_or(HubError::NoBrowser)?;
        if mode == CaptureMode::VirtualDisplay {
            let setup = self.display()?;
            config.executable = setup.chrome;
            config.capture = CaptureConfig::VirtualDisplay(VirtualDisplayConfig::new(setup.xvfb));
        }
        if let Some(root) = &config.profile_root {
            std::fs::create_dir_all(root).map_err(crate::browser::BrowserError::from)?;
        }
        let server = Arc::new(BrowserServer::launch(config).await?);
        *slot = Some(server.clone());
        Ok(server)
    }

    /// The tab behind `uri`, for the browser's structured tools.
    pub async fn tab(&self, uri: &SurfaceUri) -> Result<Arc<Tab>> {
        let server = self.server.lock().await.clone().ok_or_else(|| HubError::NotFound(uri.clone()))?;
        server.tab(uri).map_err(|_| HubError::NotFound(uri.clone()))
    }
}

fn mode_of(server: &BrowserServer) -> CaptureMode {
    match server.config().capture {
        CaptureConfig::Screencast => CaptureMode::Screencast,
        CaptureConfig::VirtualDisplay(_) => CaptureMode::VirtualDisplay,
    }
}

#[async_trait]
impl SurfaceProvider for BrowserProvider {
    fn name(&self) -> &'static str {
        crate::browser::SERVER_NAME
    }

    async fn open(&self, req: &OpenRequest) -> Result<Opened> {
        let OpenRequest::Url(url) = req else { return Err(unsupported(self.name(), req)) };
        let tab = self.server().await?.open_tab(&crate::hub::normalize_url(url)).await?;
        Ok(Opened::new(tab, Origin::Browser))
    }

    async fn close(&self, uri: &SurfaceUri) -> Result<()> {
        let server = self.server.lock().await.clone();
        if let Some(server) = server {
            server.close_tab(uri).await?;
        }
        Ok(())
    }

    async fn shutdown(&self) {
        let server = self.server.lock().await.take();
        if let Some(server) = server
            && let Err(error) = server.shutdown().await
        {
            tracing::warn!(%error, "stopping the browser");
        }
    }
}

/// PDF, Office and image files.
pub struct DocsProvider(DocsServer);

impl DocsProvider {
    pub(crate) fn new(fonts: Fonts) -> Self {
        Self(DocsServer::new(fonts))
    }

    pub fn document(&self, uri: &SurfaceUri) -> Result<DocSurface> {
        self.0.surface(uri).map_err(|_| HubError::NotFound(uri.clone()))
    }
}

#[async_trait]
impl SurfaceProvider for DocsProvider {
    fn name(&self) -> &'static str {
        crate::docs::SERVER_NAME
    }

    async fn open(&self, req: &OpenRequest) -> Result<Opened> {
        let OpenRequest::File(path) = req else { return Err(unsupported(self.name(), req)) };
        Ok(Opened::new(Arc::new(self.0.open_file(path).await?), Origin::Document))
    }

    async fn close(&self, uri: &SurfaceUri) -> Result<()> {
        self.0.close(uri);
        Ok(())
    }
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

/// RFB endpoints by `host:port`; they stay listed after closing for reconnects.
pub struct VncProvider {
    server: RfbServer,
    passwords: Arc<Passwords>,
}

impl VncProvider {
    pub(crate) fn new(options: RfbOptions) -> Self {
        let passwords = Arc::new(Passwords::default());
        Self { server: RfbServer::new(Vec::new(), passwords.clone(), options), passwords }
    }
}

#[async_trait]
impl SurfaceProvider for VncProvider {
    fn name(&self) -> &'static str {
        crate::vnc::SERVER_NAME
    }

    async fn list(&self) -> Vec<Listed> {
        let infos = self.server.list().await.unwrap_or_default();
        infos.into_iter().map(|info| Listed { info, origin: Origin::Vm, open: false }).collect()
    }

    async fn open(&self, req: &OpenRequest) -> Result<Opened> {
        let uri = match req {
            OpenRequest::Uri(uri) => uri.clone(),
            OpenRequest::Vm { address, password } => {
                let address = address.trim();
                let (host, port) = address.rsplit_once(':').unwrap_or_default();
                if host.is_empty() || host.contains('/') || port.parse::<u16>().is_err() {
                    return Err(HubError::Address(address.to_owned()));
                }
                match password.clone().filter(|p| !p.is_empty()) {
                    Some(password) => self.passwords.0.locked().insert(address.to_owned(), password),
                    None => self.passwords.0.locked().remove(address),
                };
                self.server.add_endpoint(RfbEndpoint { name: address.to_owned(), address: address.to_owned() })?
            }
            other => return Err(unsupported(self.name(), other)),
        };
        Ok(Opened::new(self.server.open_rfb(&uri).await?, Origin::Vm))
    }
}

/// Surfaces third party MCP servers declare, opened as [`McpSurface`]s.
#[derive(Default)]
pub struct McpDeclared {
    declared: Mutex<BTreeMap<String, Vec<SurfaceInfo>>>,
    manager: Mutex<Option<Manager>>,
}

impl McpDeclared {
    pub fn declared(&self) -> BTreeMap<String, Vec<SurfaceInfo>> {
        self.declared.locked().clone()
    }

    /// Reads the `surface://` resources of every running server of `manager`; returns the failed listings.
    pub async fn refresh(&self, manager: &Manager) -> Vec<(String, String)> {
        *self.manager.locked() = Some(manager.clone());
        let (mut declared, mut failed) = (BTreeMap::new(), Vec::new());
        for view in manager.view(&AllowAll).into_iter().filter(|v| v.status == Status::Running) {
            let name = view.spec.name;
            match manager.surfaces(&name).await {
                Ok(surfaces) => {
                    let infos: Vec<SurfaceInfo> = surfaces.into_iter().map(|(uri, r)| declared_info(uri, &r)).collect();
                    if !infos.is_empty() {
                        declared.insert(name, infos);
                    }
                }
                Err(error) => failed.push((name, error.to_string())),
            }
        }
        *self.declared.locked() = declared;
        failed
    }
}

#[async_trait]
impl SurfaceProvider for McpDeclared {
    fn name(&self) -> &'static str {
        "mcp"
    }

    async fn list(&self) -> Vec<Listed> {
        let declared = self.declared.locked();
        let all = declared.iter().flat_map(|(server, infos)| infos.iter().map(move |i| (server, i)));
        all.map(|(server, info)| Listed { info: info.clone(), origin: Origin::Mcp(server.clone()), open: false }).collect()
    }

    async fn open(&self, req: &OpenRequest) -> Result<Opened> {
        let OpenRequest::Uri(uri) = req else { return Err(unsupported(self.name(), req)) };
        let found = self.list().await.into_iter().find(|l| &l.info.uri == uri).ok_or_else(|| HubError::NotFound(uri.clone()))?;
        let Origin::Mcp(server) = found.origin else { unreachable!("declared surfaces have an MCP origin") };
        let manager = self.manager.locked().clone().ok_or(HubError::NoMcp)?;
        Ok(Opened::new(Arc::new(McpSurface::new(manager, server.clone(), found.info)), Origin::Mcp(server)))
    }
}

/// What a declared resource says about itself; without the `omp/surface` metadata it is a
/// pixel surface (a document when it has a MIME type) of unknown size.
fn declared_info(uri: SurfaceUri, resource: &loa_mcp::model::Resource) -> SurfaceInfo {
    let value = serde_json::to_value(resource).ok();
    let parsed = value.and_then(|v| crate::hub::mcp::parse_resource(&v).ok().flatten());
    parsed.unwrap_or_else(|| SurfaceInfo {
        uri,
        name: resource.title.clone().unwrap_or_else(|| resource.name.clone()),
        width: 0,
        height: 0,
        mode: if resource.mime_type.is_some() { SurfaceMode::Document } else { SurfaceMode::Pixel },
        mime: resource.mime_type.clone(),
    })
}
