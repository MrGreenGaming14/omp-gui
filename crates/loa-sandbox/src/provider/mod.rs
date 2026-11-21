//! Sandbox providers: each starts a place to work, runs the exec stub there
//! and hands back a byte channel to it. A [`ProviderFactory`] per kind
//! probes this machine and builds its provider from the settings; the
//! [`ProviderRegistry`] holds the factories.

pub mod docker;
pub mod local;
pub mod peer;
pub mod ssh;
#[cfg(feature = "vm")]
pub mod vm;

use crate::service::SandboxConfig;
use crate::{Result, SandboxError};
use async_trait::async_trait;
use loa_util::registry::Registry;
use serde::Deserialize;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};

loa_util::str_enum! {
    /// The kinds of sandbox `sandbox_task` can ask for. `Local` is a child
    /// process on the host without isolation of its own; `Vm` is qemu.
    pub SandboxKind { Local => "local", Docker => "docker", Firecracker => "firecracker", Vm => "vm", Ssh => "ssh", Peer => "peer" }
}

impl schemars::JsonSchema for SandboxKind {
    fn schema_name() -> std::borrow::Cow<'static, str> { "SandboxKind".into() }
    fn inline_schema() -> bool { true }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string", "enum": Self::VALUES })
    }
}

/// Per call options from the `sandbox_task` arguments. Each provider reads
/// the ones that apply to it and falls back to its own configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxOptions {
    /// Container image (docker).
    pub image: Option<String>,
    /// SSH host alias or `user@host` (ssh).
    pub host: Option<String>,
    /// Node key or name of the peer (peer).
    pub peer: Option<String>,
    /// Allow network access from inside the sandbox (docker, firecracker, vm).
    pub network: Option<bool>,
    pub memory_mib: Option<u32>,
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
    pub env: Vec<(String, String)>,
    pub options: SandboxOptions,
}

/// How the workspace reaches the stub's root directory.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceMode {
    /// The stub's root is the host workspace itself (local) or a mount of it (docker).
    Shared,
    /// The stub's root starts empty; [`crate::sync`] copies the workspace in and out.
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

/// What a sandbox leaves behind outside its process, and the command that
/// removes it: awaited by `stop`, and started detached when the sandbox is dropped.
pub trait Teardown: Send + Sync {
    fn command(&self) -> std::process::Command;
    /// A failure that only means there was nothing left to remove.
    fn gone(&self, _stderr: &str) -> bool { false }
}

/// A stub talking over the stdio of a host process, with an optional teardown.
pub struct ProcessSandbox {
    child: Child,
    channel: Option<Channel>,
    mode: WorkspaceMode,
    teardown: Option<Box<dyn Teardown>>,
    provider: &'static str,
}

/// Spawns `command` with the stub on its stdio.
pub fn process(provider: &'static str, mut command: Command, mode: WorkspaceMode, teardown: Option<Box<dyn Teardown>>) -> Result<ProcessSandbox> {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = command.spawn()?;
    let (reader, writer) = (child.stdout.take().expect("piped stdout"), child.stdin.take().expect("piped stdin"));
    Ok(ProcessSandbox { child, channel: Some(Channel { reader: Box::new(reader), writer: Box::new(writer) }), mode, teardown, provider })
}

impl Drop for ProcessSandbox {
    fn drop(&mut self) {
        if let Some(teardown) = self.teardown.take() {
            let _ = teardown.command().stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }
}

#[async_trait]
impl Sandbox for ProcessSandbox {
    fn take_channel(&mut self) -> Option<Channel> { self.channel.take() }
    fn workspace_mode(&self) -> WorkspaceMode { self.mode }
    async fn stop(mut self: Box<Self>) -> Result<()> {
        if let Some(teardown) = self.teardown.take()
            && let Err(error) = loa_util::exec::run_checked(Command::from(teardown.command())).await
            && !teardown.gone(&error)
        {
            return Err(SandboxError::Provider { provider: self.provider, message: format!("teardown failed ({error})") });
        }
        self.channel.take();
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
        }
        self.child.wait().await?;
        Ok(())
    }
}

/// What the providers find on this machine besides its programs.
#[derive(Clone)]
pub struct Machine {
    /// The stub binary, if one was found.
    pub stub: Option<PathBuf>,
    pub peers: Option<Arc<dyn peer::PeerLink>>,
}

impl Machine {
    pub fn this() -> Self {
        Self { stub: local::find_stub(), peers: None }
    }

    pub fn which(&self, name: &str) -> Option<PathBuf> {
        loa_util::exec::which(name)
    }

    pub fn kvm(&self) -> bool {
        loa_util::exec::kvm_available()
    }
}

/// Whether a provider can be used on this machine now.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    Ready,
    /// Installed, but it needs settings (an image, a host, a grant).
    NotConfigured,
    /// Missing software, hardware or permission.
    Unavailable,
}

/// One provider's availability with the reason, as the Tools panel shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderStatus {
    pub kind: SandboxKind,
    pub availability: Availability,
    pub detail: String,
}

pub(crate) fn status(kind: SandboxKind, availability: Availability, detail: impl Into<String>) -> ProviderStatus {
    ProviderStatus { kind, availability, detail: detail.into() }
}

pub(crate) const NO_STUB: &str = "loa-sandbox-stub was not found next to the app or on PATH (set LOA_SANDBOX_STUB)";

/// Probes one kind of provider and builds it from the settings.
#[async_trait]
pub trait ProviderFactory: Send + Sync {
    fn kind(&self) -> SandboxKind;
    /// Whether the provider can run here, and why not.
    async fn probe(&self, machine: &Machine, config: &SandboxConfig) -> ProviderStatus;
    /// The provider, when this machine has the programs and settings for it.
    /// It can still fail at run time (a stopped daemon, an unreachable host).
    fn build(&self, machine: &Machine, config: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>>;
}

/// The provider factories by kind.
#[derive(Clone)]
pub struct ProviderRegistry(Registry<dyn ProviderFactory>);

impl ProviderRegistry {
    /// Every built in provider this build has.
    pub fn builtin() -> Self {
        let mut registry = Self(Registry::new());
        registry.register(Arc::new(local::Factory));
        registry.register(Arc::new(docker::Factory));
        registry.register(Arc::new(ssh::Factory));
        registry.register(Arc::new(peer::Factory));
        #[cfg(feature = "vm")]
        registry.register(Arc::new(vm::Firecracker)).register(Arc::new(vm::Qemu));
        registry
    }

    pub fn register(&mut self, factory: Arc<dyn ProviderFactory>) -> &mut Self {
        self.0.register(factory.kind().as_str(), factory);
        self
    }

    /// The factories in [`SandboxKind`] order, as the app lists them.
    pub fn factories(&self) -> impl Iterator<Item = &Arc<dyn ProviderFactory>> {
        let mut factories: Vec<_> = self.0.iter().map(|(_, factory)| factory).collect();
        factories.sort_by_key(|factory| factory.kind());
        factories.into_iter()
    }
}
