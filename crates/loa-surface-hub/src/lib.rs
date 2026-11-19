//! The app's surface service.
//!
//! - [`SurfaceHub`] owns the built-in surface servers (the `browser` on the
//!   installed Chrome or Chromium, `docs` for PDF, Office and image files,
//!   `vm` for RFB endpoints), keeps the surfaces the user or the agent opened,
//!   and lists the surfaces third party MCP servers declare, which it opens as
//!   [`McpSurface`]s.
//! - [`SurfaceTools`] runs the `surface_*` agent tools on the hub's surfaces,
//!   as host tools ([`SurfaceTools::host_tools`]) or behind the loopback MCP
//!   endpoint ([`McpEndpoint`]) for harnesses that connect to MCP servers.
//! - [`dom`] builds the `<surface>` elements recorded in a session.

pub mod dom;
mod endpoint;
mod hub;
mod mcp_surface;
mod tools;

pub use endpoint::{McpEndpoint, SERVER_PREFIX};
pub use hub::{HubConfig, HubEvent, Listed, Opened, Origin, SurfaceHub, normalize_url, user_namespaces};
pub use mcp_surface::McpSurface;
pub use tools::{SurfaceTools, ToolContent, ToolResult, tool_definitions};

use loa_surface::{SurfaceError, SurfaceUri};

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
    #[error(transparent)]
    Surface(#[from] SurfaceError),
    #[error(transparent)]
    Browser(#[from] loa_surface_browser::BrowserError),
    #[error(transparent)]
    Docs(#[from] loa_surface_docs::DocsError),
    #[error("MCP: {0}")]
    Mcp(String),
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
