//! What the app holds: the sandbox settings, which providers this machine can
//! run (and why not), and the `sandbox_task` host tool for each new session.

use crate::provider::peer::PeerLink;
use crate::provider::{Machine, ProviderRegistry, ProviderStatus, SandboxProvider};
use crate::tool::{SandboxEvent, SandboxTask};
use crate::worker::WorkerJournal;
use loa_contracts::harness::{Adapter, HostToolSpec, HostTools, SessionConfig};
use loa_contracts::tools::{DynTool, ToolSet};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Sandbox settings, kept by the app as JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// Image used when a docker call names none. The host stub is mounted
    /// into it, so it needs a C library compatible with the host's.
    pub docker_image: String,
    pub docker_network: bool,
    /// Guest kernel and root filesystem for Firecracker (feature `vm`). The
    /// rootfs init must put ttyS0 in raw mode and run `loa-sandbox-stub /workspace` on it.
    pub firecracker_kernel: Option<PathBuf>,
    pub firecracker_rootfs: Option<PathBuf>,
    /// Raw disk image for qemu (feature `vm`), whose init runs the stub on the
    /// first serial port like the Firecracker rootfs.
    pub vm_disk: Option<PathBuf>,
    pub vm_kernel: Option<PathBuf>,
    /// SSH hosts the agent may use. Empty: the hosts of `~/.ssh/config`.
    pub ssh_hosts: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        let docker_image = "debian:stable-slim".into();
        Self { docker_image, docker_network: false, firecracker_kernel: None, firecracker_rootfs: None, vm_disk: None, vm_kernel: None, ssh_hosts: Vec::new() }
    }
}

impl SandboxConfig {
    /// Reads the settings; a missing file gives the defaults.
    pub fn load(path: &Path) -> crate::Result<Self> {
        Ok(loa_util::fs::read_optional(path)?.map(|bytes| serde_json::from_slice(&bytes)).transpose()?.unwrap_or_default())
    }

    pub fn save(&self, path: &Path) -> crate::Result<()> {
        Ok(loa_util::fs::write_json_atomic(path, self)?)
    }
}

/// The app's sandbox service: settings, provider availability and the
/// `sandbox_task` host tool for sessions.
pub struct SandboxService {
    config: SandboxConfig,
    journal: Arc<dyn WorkerJournal>,
    events: broadcast::Sender<SandboxEvent>,
    machine: Machine,
    registry: ProviderRegistry,
}

impl SandboxService {
    /// `journal` keeps the worker sessions on the host.
    pub fn new(config: SandboxConfig, journal: Arc<dyn WorkerJournal>) -> Self {
        Self { config, journal, events: broadcast::channel(256).0, machine: Machine::this(), registry: ProviderRegistry::builtin() }
    }

    /// Uses this stub binary (`None`: there is none).
    pub fn with_stub(mut self, stub: Option<PathBuf>) -> Self {
        self.machine.stub = stub;
        self
    }

    /// Enables the peer provider.
    pub fn with_peers(mut self, peers: Arc<dyn PeerLink>) -> Self {
        self.machine.peers = Some(peers);
        self
    }

    /// Probes and runs on `machine` instead of this one.
    pub fn with_machine(mut self, machine: Machine) -> Self {
        self.machine = machine;
        self
    }

    pub fn config(&self) -> &SandboxConfig { &self.config }

    /// New settings apply to sessions started afterwards.
    pub fn set_config(&mut self, config: SandboxConfig) { self.config = config; }

    /// The stub binary, for the peer side ([`crate::provider::peer::serve_stub`]).
    pub fn stub(&self) -> Option<&Path> { self.machine.stub.as_deref() }

    /// Every `sandbox_task` run of every session.
    pub fn subscribe(&self) -> broadcast::Receiver<SandboxEvent> { self.events.subscribe() }

    /// The providers this machine has the programs and settings for.
    pub fn providers(&self) -> Vec<Arc<dyn SandboxProvider>> {
        self.registry.factories().filter_map(|factory| factory.build(&self.machine, &self.config)).collect()
    }

    /// The `sandbox_task` host tool for a new session, whose worker runs on
    /// `adapter` with `template`'s settings in `template.cwd`. `None` when no
    /// provider can run here.
    pub fn host_tool(&self, adapter: Arc<dyn Adapter>, template: &SessionConfig) -> Option<(HostToolSpec, Arc<dyn HostTools>)> {
        let providers = self.providers();
        if providers.is_empty() {
            return None;
        }
        // The worker gets the sandbox tools instead; nothing of the parent's host tools may reach it.
        let template = SessionConfig { host_tools: Vec::new(), host_tool_handler: None, ..template.clone() };
        let tool = SandboxTask::new(adapter, template, Arc::clone(&self.journal), providers).with_events(self.events.clone());
        // Only the kinds this machine runs are offered.
        let mut spec = DynTool::spec(&tool);
        if let Ok(mut schema) = serde_json::from_str::<serde_json::Value>(&spec.input_schema) {
            schema["properties"]["kind"]["enum"] = serde_json::json!(tool.kinds());
            spec.input_schema = schema.to_string();
        }
        let mut tools = ToolSet::new();
        tools.register(tool);
        Some((spec, Arc::new(tools)))
    }

    /// Checks every provider kind on this machine. Owns what it needs, so it
    /// can run as a background task.
    pub fn probe(&self) -> impl Future<Output = Vec<ProviderStatus>> + Send + 'static {
        let (machine, config, registry) = (self.machine.clone(), self.config.clone(), self.registry.clone());
        async move {
            let mut statuses = Vec::new();
            for factory in registry.factories() {
                statuses.push(factory.probe(&machine, &config).await);
            }
            statuses
        }
    }
}
