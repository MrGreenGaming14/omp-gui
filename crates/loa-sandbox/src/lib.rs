//! `sandbox_task`: runs a task as a worker session whose commands and file
//! operations happen inside a sandbox (Docker, an SSH host, a peer node or,
//! with feature `vm`, Firecracker or qemu), while the worker's model calls,
//! keys and journal stay on the host.
//!
//! - [`proto`]: the wire protocol spoken with the exec stub (the stub builds
//!   this crate with default features off and gets only this module)
//! - [`client`]: the host side of that protocol
//! - [`provider`]: [`SandboxProvider`], the provider factories and their registry
//! - [`isolation`]: isolated checkouts and patch capture
//! - [`sync`]: copying the workspace in and out of sandboxes that don't share it
//! - [`worker`]: the worker session and its `sandbox_*` tools
//! - [`tool`]: the `sandbox_task` tool and the events of its runs
//! - [`service`]: what the app holds: settings, provider availability and the
//!   host tool for each session
//! - [`runs`]: the runs the app lists, built from the tool's events

pub mod proto;

#[cfg(feature = "host")]
mod host {
    pub use crate::client::{ExecOutput, StubClient};
    pub use crate::provider::{Availability, Channel, ProviderStatus, Sandbox, SandboxKind, SandboxOptions, SandboxProvider, SandboxSpec, WorkspaceMode};
    pub use crate::runs::{RunEntry, RunLog, RunState};
    pub use crate::service::{SandboxConfig, SandboxService};
    pub use crate::tool::{SandboxEvent, SandboxRun, SandboxTaskResult};

    /// Errors of the host side.
    #[derive(Debug, thiserror::Error)]
    pub enum SandboxError {
        #[error(transparent)]
        Io(#[from] std::io::Error),
        /// The stub answered a request with an error.
        #[error("sandbox: {0}")]
        Stub(String),
        #[error("the sandbox stub disconnected")]
        Disconnected,
        /// A provider couldn't start, reach or stop its sandbox.
        #[error("{provider}: {message}")]
        Provider { provider: &'static str, message: String },
        #[error("isolation: {0}")]
        Isolation(String),
        #[error("invalid arguments: {0}")]
        Args(String),
        #[error("invalid sandbox settings: {0}")]
        Settings(#[from] serde_json::Error),
        #[error("no provider for sandbox kind `{0}`")]
        NoProvider(String),
        #[error(transparent)]
        Adapter(#[from] loa_contracts::harness::AdapterError),
    }

    pub type Result<T, E = SandboxError> = std::result::Result<T, E>;
}

#[cfg(feature = "host")]
pub use host::*;
#[cfg(feature = "host")]
pub mod client;
#[cfg(feature = "host")]
pub mod isolation;
#[cfg(feature = "host")]
pub mod provider;
#[cfg(feature = "host")]
pub mod runs;
#[cfg(feature = "host")]
pub mod service;
#[cfg(feature = "host")]
pub mod sync;
#[cfg(feature = "host")]
pub mod tool;
#[cfg(feature = "host")]
pub mod worker;
