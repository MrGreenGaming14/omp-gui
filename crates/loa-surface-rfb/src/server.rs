//! The `vm` surface server: configured RFB endpoints as surfaces.

use crate::{PasswordSource, RfbSurface};
use async_trait::async_trait;
use loa_surface::{Surface, SurfaceError, SurfaceInfo, SurfaceMode, SurfaceServer, SurfaceUri};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

/// Server part of the uris, `surface://vm/<name>`.
pub const SERVER_NAME: &str = "vm";

/// One VNC server the user configured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RfbEndpoint {
    /// Surface name, the last part of `surface://vm/<name>`.
    pub name: String,
    /// `host:port` of the RFB server.
    pub address: String,
}

/// Connection tuning.
#[derive(Clone, Debug)]
pub struct RfbOptions {
    /// Limit for TCP connect plus handshake.
    pub connect_timeout: Duration,
    /// Wait before the first reconnect; doubles per failed attempt.
    pub backoff_initial: Duration,
    /// Longest wait between reconnect attempts.
    pub backoff_max: Duration,
    /// Scroll distance in surface pixels that makes one wheel click.
    pub scroll_step: f32,
    /// Frames a subscriber may fall behind before it lags.
    pub frame_capacity: usize,
    /// ClientInit shared flag: leave other viewers connected.
    pub shared: bool,
}

impl Default for RfbOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            backoff_initial: Duration::from_millis(250),
            backoff_max: Duration::from_secs(30),
            scroll_step: 40.0,
            frame_capacity: 16,
            shared: true,
        }
    }
}

/// Serves each [`RfbEndpoint`] as the pixel surface `surface://vm/<name>`.
/// Opening a surface that is already open returns the same connection.
pub struct RfbServer {
    endpoints: Vec<RfbEndpoint>,
    passwords: Arc<dyn PasswordSource>,
    options: RfbOptions,
    open: Mutex<HashMap<String, Weak<RfbSurface>>>,
}

impl RfbServer {
    pub fn new(
        endpoints: Vec<RfbEndpoint>,
        passwords: Arc<dyn PasswordSource>,
        options: RfbOptions,
    ) -> Self {
        Self {
            endpoints,
            passwords,
            options,
            open: Mutex::new(HashMap::new()),
        }
    }

    fn live(&self, name: &str) -> Option<Arc<RfbSurface>> {
        // The map holds plain data, a poisoned lock leaves it usable.
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .and_then(Weak::upgrade)
    }

    /// Opens `uri` with the RFB specific API (cursor shape, connection state).
    pub async fn open_rfb(&self, uri: &SurfaceUri) -> loa_surface::Result<Arc<RfbSurface>> {
        let not_found = || SurfaceError::NotFound(uri.clone());
        if uri.server() != SERVER_NAME {
            return Err(not_found());
        }
        let endpoint = self
            .endpoints
            .iter()
            .find(|e| e.name == uri.name())
            .ok_or_else(not_found)?;
        if let Some(surface) = self.live(&endpoint.name) {
            return Ok(surface);
        }
        let surface = Arc::new(
            RfbSurface::connect(
                uri.clone(),
                endpoint.clone(),
                Arc::clone(&self.passwords),
                self.options.clone(),
            )
            .await?,
        );
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        // A concurrent open may have connected first; keep its connection.
        if let Some(existing) = open.get(&endpoint.name).and_then(Weak::upgrade) {
            return Ok(existing);
        }
        open.insert(endpoint.name.clone(), Arc::downgrade(&surface));
        Ok(surface)
    }
}

#[async_trait]
impl SurfaceServer for RfbServer {
    fn name(&self) -> &str {
        SERVER_NAME
    }

    /// Every configured endpoint. A surface that is not open reports a 0x0
    /// size, since the size is only known after connecting.
    async fn list(&self) -> loa_surface::Result<Vec<SurfaceInfo>> {
        self.endpoints
            .iter()
            .map(|e| match self.live(&e.name) {
                Some(surface) => Ok(surface.info()),
                None => Ok(SurfaceInfo {
                    uri: SurfaceUri::new(SERVER_NAME, &e.name)?,
                    name: e.name.clone(),
                    width: 0,
                    height: 0,
                    mode: SurfaceMode::Pixel,
                    mime: None,
                }),
            })
            .collect()
    }

    async fn open(&self, uri: &SurfaceUri) -> loa_surface::Result<Arc<dyn Surface>> {
        Ok(self.open_rfb(uri).await?)
    }
}
