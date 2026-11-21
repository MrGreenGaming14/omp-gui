//! MCP client, server registry and config import, plus the toolkit that
//! serves app tools over MCP and the omp tools bridge.
//!
//! - [`Manager`] runs registered servers (stdio children and streamable HTTP),
//!   restarting crashed ones with backoff; [`Registry`] validates, saves and
//!   rolls back edits through the [`ServerStore`] port.
//! - [`import`] reads other apps' MCP configs through [`import::ImportSource`] adapters.
//! - [`ToolFilter`] is the per project toggle and per peer grant check.
//! - [`host_tools`] hands running servers' tools to harnesses that take host tools.
//! - [`toolkit`] serves a `ToolSet` over MCP; [`bridge`] serves omp's tools that way.

pub mod bridge;
pub mod central;
mod connect;
mod error;
pub mod host_tools;
pub mod import;
mod manager;
mod registry;
mod spec;
pub mod toolkit;

pub use error::{Error, Result};
pub use manager::{Backoff, Event, Manager, Options, ServerView, Status, ToolView, surface_uri};
pub use registry::{MemoryStore, Registry, ServerStore, StoreError};
pub use spec::{HttpSpec, ServerSpec, StdioSpec, Transport, parse_header, validate_name};
pub use toolkit::ToolSetMcp;

/// MCP protocol types returned by the client: tools, call results, resources.
pub use rmcp::model;

/// Decides whether a tool may be listed and called: the per project toggles, and for a peer, its grant.
pub trait ToolFilter: Send + Sync {
    fn allows(&self, server: &str, tool: &str) -> bool;
}

impl<F: Fn(&str, &str) -> bool + Send + Sync> ToolFilter for F {
    fn allows(&self, server: &str, tool: &str) -> bool {
        self(server, tool)
    }
}

/// Lets every tool through.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllowAll;

impl ToolFilter for AllowAll {
    fn allows(&self, _: &str, _: &str) -> bool {
        true
    }
}
