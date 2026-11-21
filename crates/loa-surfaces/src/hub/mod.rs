//! The app's surface service: [`SurfaceHub`] over the [`SurfaceProvider`]s (browser, docs,
//! vm, MCP declared), the `surface_*` agent tools ([`SurfaceTools`]) as host tools or behind
//! the loopback [`McpEndpoint`], and the `<surface>` session elements ([`dom`]).

pub mod dom;
mod endpoint;
mod mcp;
mod mcp_surface;
mod providers;
mod service;
mod tools;
#[cfg(feature = "remote-video")]
mod video;

pub use endpoint::{McpEndpoint, SERVER_PREFIX};
pub use service::{
    CaptureMode, HubConfig, HubEvent, Listed, OpenRequest, Opened, Origin, SurfaceHub, SurfaceProvider, VirtualDisplaySetup,
    normalize_url,
};
pub use tools::{SurfaceTools, tool_set};
#[cfg(feature = "remote-video")]
pub use {crate::proto::video::LiveVideo, video::{RemoteVideo, VideoConfig}};

use loa_contracts::surface::{SurfaceError, SurfaceUri};

#[derive(Debug, thiserror::Error)]
pub enum HubError {
    #[error("no Chrome or Chromium is installed (looked on PATH, in the usual install locations and in ~/.cache/ms-playwright)")]
    NoBrowser,
    #[error("`{0}` isn't a host:port address")]
    Address(String),
    #[error("no surface {0}")]
    NotFound(SurfaceUri),
    #[error("the MCP servers aren't connected")]
    NoMcp,
    #[error("the virtual display can't run here: {0}")]
    NoVirtualDisplay(String),
    #[error(transparent)]
    Surface(#[from] SurfaceError),
    #[error(transparent)]
    Browser(#[from] crate::browser::BrowserError),
    #[error(transparent)]
    Docs(#[from] crate::docs::DocsError),
    #[error("{0}")]
    Unsupported(String),
}

pub type Result<T, E = HubError> = std::result::Result<T, E>;

impl From<HubError> for SurfaceError {
    fn from(error: HubError) -> Self {
        match error {
            HubError::Surface(inner) => inner,
            HubError::NotFound(uri) => SurfaceError::NotFound(uri),
            other => SurfaceError::Backend(other.to_string()),
        }
    }
}
