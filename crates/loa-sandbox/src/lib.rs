//! `sandbox_task`: runs a task as a worker session whose commands and file
//! operations happen inside a sandbox (Docker, an SSH host, a peer node or,
//! with feature `vm`, Firecracker or qemu), while the worker's model calls,
//! keys and journal stay on the host.
//!
//! [`proto`] is all the stub needs; it builds this crate without the `host`
//! feature, which brings everything else.

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

/// Declares the modules only the host has.
macro_rules! host_modules {
    ($($module:ident),*) => { $(#[cfg(feature = "host")] pub mod $module;)* };
}

#[cfg(feature = "host")]
pub use host::*;
host_modules!(client, isolation, provider, runs, service, sync, tool, worker);
