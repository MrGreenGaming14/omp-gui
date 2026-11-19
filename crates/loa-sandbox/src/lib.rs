//! `sandbox_task`: runs a task as a worker session whose commands and file
//! operations happen inside a sandbox (Docker, Firecracker, a VM, an SSH host
//! or a peer node), while the worker's model calls, keys and journal stay on
//! the host.
//!
//! - [`proto`]: the wire protocol spoken with the exec stub (std only; the
//!   stub builds this crate with default features off)
//! - [`client`]: the host side of that protocol
//! - [`provider`]: [`SandboxProvider`] and the built in providers
//! - [`isolation`]: isolated checkouts (ovnfs, git worktree, copy) and patch capture
//! - [`sync`]: copying the workspace in and out of sandboxes that don't share it
//! - [`worker`]: the worker session and the sandbox backed tools it gets
//! - [`tool`]: the `sandbox_task` host tool and the events of its runs
//! - [`service`]: what the app holds: settings, provider availability and the
//!   host tool for each session
//! - [`runs`]: the runs the app lists, built from the tool's events

pub mod proto;

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

#[cfg(feature = "host")]
pub use client::{ExecOutput, StubClient};
#[cfg(feature = "host")]
pub use provider::{Channel, Sandbox, SandboxKind, SandboxOptions, SandboxProvider, SandboxSpec, WorkspaceMode};
#[cfg(feature = "host")]
pub use service::{Availability, ProviderStatus, SandboxConfig, SandboxService};
#[cfg(feature = "host")]
pub use runs::{RunEntry, RunLog, RunState};
#[cfg(feature = "host")]
pub use tool::{SandboxEvent, SandboxRun, SandboxTaskResult};

/// Errors of the host side.
#[cfg(feature = "host")]
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Protocol(#[from] proto::ProtoError),
    /// The stub answered a request with an error.
    #[error("sandbox: {0}")]
    Stub(String),
    /// The stub went away.
    #[error("the sandbox stub disconnected")]
    Disconnected,
    /// A provider couldn't start, reach or stop its sandbox.
    #[error("{provider}: {message}")]
    Provider { provider: &'static str, message: String },
    #[error("isolation: {0}")]
    Isolation(String),
    #[error("invalid arguments: {0}")]
    Args(String),
    #[error("no provider for sandbox kind `{0}`")]
    NoProvider(String),
    #[error(transparent)]
    Adapter(#[from] loa_adapter::AdapterError),
}

#[cfg(feature = "host")]
pub type Result<T, E = SandboxError> = std::result::Result<T, E>;

#[cfg(all(test, feature = "host"))]
mod tests {
    use async_trait::async_trait;
    use loa_adapter::{Adapter, AdapterError, Capabilities, HarnessSession, SessionConfig};
    use loa_core::{Dom, PatchSink};
    use std::sync::Arc;

    /// An adapter for tests that never start a worker.
    pub struct NoAdapter;

    #[async_trait]
    impl Adapter for NoAdapter {
        fn harness(&self) -> &'static str { "none" }
        fn capabilities(&self) -> Capabilities { Capabilities::default() }
        async fn start(&self, _: SessionConfig, _: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
            Err(AdapterError::Format("no harness in this test".into()))
        }
        async fn resume(&self, _: SessionConfig, _: &Dom, _: Arc<dyn PatchSink>) -> loa_adapter::Result<Box<dyn HarnessSession>> {
            Err(AdapterError::Format("no harness in this test".into()))
        }
    }
}
