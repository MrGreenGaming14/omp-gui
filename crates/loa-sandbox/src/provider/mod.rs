//! Sandbox providers: each starts a place to work, runs the exec stub there
//! and hands back a byte channel to it.

pub mod docker;
pub mod firecracker;
pub mod local;
pub mod peer;
pub mod qemu;
mod serial;
pub mod ssh;

use crate::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite};

/// The kinds of sandbox `sandbox_task` can ask for.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxKind {
    /// A child process on the host. No isolation of its own; the reference
    /// provider and the one tests use.
    Local,
    Docker,
    Firecracker,
    /// A generic VM through qemu.
    Vm,
    Ssh,
    Peer,
}

impl SandboxKind {
    pub const ALL: [SandboxKind; 6] = [
        SandboxKind::Local,
        SandboxKind::Docker,
        SandboxKind::Firecracker,
        SandboxKind::Vm,
        SandboxKind::Ssh,
        SandboxKind::Peer,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SandboxKind::Local => "local",
            SandboxKind::Docker => "docker",
            SandboxKind::Firecracker => "firecracker",
            SandboxKind::Vm => "vm",
            SandboxKind::Ssh => "ssh",
            SandboxKind::Peer => "peer",
        }
    }
}

/// Per call options from the `sandbox_task` arguments. Each provider reads
/// the ones that apply to it and falls back to its own configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxOptions {
    /// Container image (docker).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// SSH host alias or `user@host` (ssh).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Node key or name of the peer (peer).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<String>,
    /// Allow network access from inside the sandbox (docker, firecracker, vm).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mib: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
}

/// What a provider needs to start one sandbox.
#[derive(Clone, Debug)]
pub struct SandboxSpec {
    /// Unique per run: names containers and temp files.
    pub name: String,
    /// Host directory the worker works on.
    pub workspace: PathBuf,
    /// The whole environment the stub (and every command it runs) gets.
    /// Nothing from the host's own environment is passed on.
    pub env: Vec<(String, String)>,
    pub options: SandboxOptions,
}

/// How the workspace reaches the stub's root directory.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceMode {
    /// The stub's root is the host workspace itself (local) or a mount of it
    /// (docker).
    Shared,
    /// The stub's root starts empty. The host copies the workspace in before
    /// the run and the changes back after it (see [`crate::sync`]).
    Copied,
}

/// Byte streams to and from a running stub.
pub struct Channel {
    pub reader: Box<dyn AsyncRead + Send + Unpin>,
    pub writer: Box<dyn AsyncWrite + Send + Unpin>,
}

/// Starts sandboxes of one kind.
#[async_trait]
pub trait SandboxProvider: Send + Sync {
    fn kind(&self) -> SandboxKind;

    /// What this sandbox is and which options it takes, for the agent.
    fn describe(&self) -> String;

    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>>;
}

/// One running sandbox with the stub started in it.
#[async_trait]
pub trait Sandbox: Send {
    /// The channel to the stub. `None` after the first call.
    fn take_channel(&mut self) -> Option<Channel>;

    fn workspace_mode(&self) -> WorkspaceMode;

    /// Stops the sandbox and removes what the provider created for it.
    /// Dropping a sandbox without calling this cleans up on a best effort basis.
    async fn stop(self: Box<Self>) -> Result<()>;
}

/// Whether this process may use `/dev/kvm` (read and write access).
pub fn kvm_available() -> bool {
    std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok()
}
