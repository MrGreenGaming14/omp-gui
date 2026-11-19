//! MCP client, server registry and config import.
//!
//! - [`Manager`] runs registered MCP servers (stdio children and streamable
//!   HTTP), tracks their status, lists their tools and resources, calls tools
//!   and restarts crashed servers with backoff.
//! - [`Registry`] adds, edits and removes servers, validating each one by
//!   connecting before it is saved through the [`ServerStore`] port.
//! - [`import`] reads the MCP configs of Claude Code, Cursor and Codex into
//!   [`ServerSpec`]s without ever writing to them.
//! - [`ToolFilter`] is the per project toggle and per peer grant check the
//!   app applies over `(server, tool)`.

mod connect;
mod error;
mod filter;
pub mod import;
mod manager;
mod registry;
mod spec;

pub use error::{Error, Result};
pub use filter::{AllowAll, ToolFilter};
pub use manager::{Backoff, Event, Manager, Options, ServerView, Status, ToolView, surface_uri};
pub use registry::{MemoryStore, Registry, ServerStore, StoreError};
pub use spec::{HttpSpec, ServerSpec, StdioSpec, Transport, parse_header, validate_name};

/// MCP protocol types returned by the client: tools, call results, resources.
pub use rmcp::model;
