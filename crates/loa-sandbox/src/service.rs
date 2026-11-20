//! What the app holds: the sandbox settings, which providers this machine can
//! run (and why not), and the `sandbox_task` host tool for each new session.

use crate::isolation;
use crate::provider::docker::{DockerProvider, DockerStub};
use crate::provider::firecracker::FirecrackerProvider;
use crate::provider::local::{LocalProvider, find_stub};
use crate::provider::peer::{PeerLink, PeerProvider};
use crate::provider::qemu::QemuProvider;
use crate::provider::ssh::SshProvider;
use crate::provider::{SandboxKind, SandboxProvider};
use crate::tool::{SandboxEvent, SandboxTask};
use crate::worker::WorkerJournal;
use loa_adapter::{Adapter, HostToolSpec, HostTools, SessionConfig};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// Sandbox settings, kept by the app as JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// Image used when a docker call names none. The host stub is mounted
    /// into it, so it needs a C library compatible with the host's.
    pub docker_image: String,
    pub docker_network: bool,
    /// Guest kernel and root filesystem for Firecracker. The rootfs init must
    /// put ttyS0 in raw mode and run `loa-sandbox-stub /workspace` on it.
    pub firecracker_kernel: Option<PathBuf>,
    pub firecracker_rootfs: Option<PathBuf>,
    /// Raw disk image for qemu, whose init runs the stub on the first serial
    /// port like the Firecracker rootfs.
    pub vm_disk: Option<PathBuf>,
    pub vm_kernel: Option<PathBuf>,
    /// SSH hosts the agent may use. Empty: the hosts of `~/.ssh/config`.
    pub ssh_hosts: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            docker_image: "debian:stable-slim".into(),
            docker_network: false,
            firecracker_kernel: None,
            firecracker_rootfs: None,
            vm_disk: None,
            vm_kernel: None,
            ssh_hosts: Vec::new(),
        }
    }
}

impl SandboxConfig {
    /// Reads the settings; a missing file gives the defaults.
    pub fn load(path: &Path) -> crate::Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, path: &Path) -> crate::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
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

/// Where the probes look. The defaults are this machine's.
#[derive(Clone, Debug)]
struct Machine {
    /// Directories searched for `docker`, `firecracker`, `qemu-system-*`, `ssh`.
    path: Option<OsString>,
    kvm_device: PathBuf,
    ssh_config: Option<PathBuf>,
}

impl Machine {
    fn this() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self {
            path: std::env::var_os("PATH"),
            kvm_device: "/dev/kvm".into(),
            ssh_config: home.map(|home| home.join(".ssh").join("config")),
        }
    }

    fn which(&self, name: &str) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let paths = self.path.as_ref()?;
        std::env::split_paths(paths)
            .map(|dir| dir.join(name))
            .find(|file| file.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0))
    }

    fn kvm(&self) -> bool {
        std::fs::OpenOptions::new().read(true).write(true).open(&self.kvm_device).is_ok()
    }

    fn ssh_config_hosts(&self) -> Vec<String> {
        self.ssh_config.as_deref().and_then(|path| std::fs::read_to_string(path).ok()).map(|text| ssh_config_hosts(&text)).unwrap_or_default()
    }
}

/// The concrete host aliases of an OpenSSH client config (no patterns).
pub fn ssh_config_hosts(config: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    for line in config.lines() {
        let line = line.trim();
        let Some((keyword, rest)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else { continue };
        if !keyword.eq_ignore_ascii_case("host") {
            continue;
        }
        for host in rest.split_whitespace() {
            if !host.contains(['*', '?', '!']) && !hosts.iter().any(|known| known == host) {
                hosts.push(host.to_owned());
            }
        }
    }
    hosts
}

const QEMU: &str = match std::env::consts::ARCH.as_bytes() {
    b"aarch64" => "qemu-system-aarch64",
    _ => "qemu-system-x86_64",
};

/// The app's sandbox service: settings, provider availability and the
/// `sandbox_task` host tool for sessions.
pub struct SandboxService {
    config: SandboxConfig,
    stub: Option<PathBuf>,
    journal: Arc<dyn WorkerJournal>,
    peers: Option<Arc<dyn PeerLink>>,
    events: broadcast::Sender<SandboxEvent>,
    machine: Machine,
}

impl SandboxService {
    /// `journal` keeps the worker sessions on the host. The stub is looked up
    /// with [`find_stub`].
    pub fn new(config: SandboxConfig, journal: Arc<dyn WorkerJournal>) -> Self {
        Self {
            config,
            stub: find_stub(),
            journal,
            peers: None,
            events: broadcast::channel(256).0,
            machine: Machine::this(),
        }
    }

    /// Uses this stub binary (`None`: there is none).
    pub fn with_stub(mut self, stub: Option<PathBuf>) -> Self {
        self.stub = stub;
        self
    }

    /// Enables the peer provider.
    pub fn with_peers(mut self, peers: Arc<dyn PeerLink>) -> Self {
        self.peers = Some(peers);
        self
    }

    /// Looks for programs in `path`, KVM at `kvm_device` and SSH hosts in
    /// `ssh_config` instead of this machine's.
    pub fn with_machine(mut self, path: Option<OsString>, kvm_device: PathBuf, ssh_config: Option<PathBuf>) -> Self {
        self.machine = Machine { path, kvm_device, ssh_config };
        self
    }

    pub fn config(&self) -> &SandboxConfig { &self.config }

    /// New settings apply to sessions started afterwards.
    pub fn set_config(&mut self, config: SandboxConfig) { self.config = config; }

    /// The stub binary, for the peer side ([`crate::provider::peer::serve_stub`]).
    pub fn stub(&self) -> Option<&Path> { self.stub.as_deref() }

    /// Every `sandbox_task` run of every session.
    pub fn subscribe(&self) -> broadcast::Receiver<SandboxEvent> { self.events.subscribe() }

    fn ssh_hosts(&self) -> Vec<String> {
        if self.config.ssh_hosts.is_empty() { self.machine.ssh_config_hosts() } else { self.config.ssh_hosts.clone() }
    }

    /// The providers this machine has the programs and settings for. A
    /// provider can still fail at run time (a stopped Docker daemon, an
    /// unreachable host); [`SandboxService::probe`] checks further.
    pub fn providers(&self) -> Vec<Arc<dyn SandboxProvider>> {
        let mut providers: Vec<Arc<dyn SandboxProvider>> = Vec::new();
        if let Some(stub) = &self.stub {
            providers.push(Arc::new(LocalProvider::new(stub)));
            if let Some(docker) = self.machine.which("docker") {
                let mut provider = DockerProvider::new(&self.config.docker_image, DockerStub::Mount(stub.clone()));
                provider.docker = docker;
                provider.network = self.config.docker_network;
                providers.push(Arc::new(provider));
            }
            if let Some(ssh) = self.machine.which("ssh") {
                let mut provider = SshProvider::new(stub, self.ssh_hosts());
                provider.ssh = ssh;
                providers.push(Arc::new(provider));
            }
        }
        if let (Some(firecracker), Some(kernel), Some(rootfs)) =
            (self.machine.which("firecracker"), &self.config.firecracker_kernel, &self.config.firecracker_rootfs)
        {
            let mut provider = FirecrackerProvider::new(kernel, rootfs);
            provider.firecracker = firecracker;
            providers.push(Arc::new(provider));
        }
        if let (Some(qemu), Some(disk)) = (self.machine.which(QEMU), &self.config.vm_disk) {
            let mut provider = QemuProvider::new(disk);
            provider.qemu = qemu;
            provider.kernel.clone_from(&self.config.vm_kernel);
            provider.kvm = self.machine.kvm();
            providers.push(Arc::new(provider));
        }
        if let Some(peers) = self.peers.as_ref().filter(|link| link.running()) {
            providers.push(Arc::new(PeerProvider::new(Arc::clone(peers))));
        }
        providers
    }

    /// The `sandbox_task` host tool for a new session, whose worker runs on
    /// `adapter` with `template`'s settings in `template.cwd`. `None` when no
    /// provider can run here.
    pub fn host_tool(&self, adapter: Arc<dyn Adapter>, template: &SessionConfig) -> Option<(HostToolSpec, Arc<dyn HostTools>)> {
        let providers = self.providers();
        if providers.is_empty() {
            return None;
        }
        let mut template = template.clone();
        // The worker gets the sandbox tools instead; nothing of the parent's
        // host tools may reach it.
        template.host_tools.clear();
        template.host_tool_handler = None;
        let workspace = template.cwd.clone();
        let mut tool = SandboxTask::new(adapter, template, Arc::clone(&self.journal), workspace).with_events(self.events.clone());
        for provider in providers {
            tool.register(provider);
        }
        Some((tool.spec(), Arc::new(tool)))
    }

    /// Checks every provider kind on this machine. Owns what it needs, so it
    /// can run as a background task.
    pub fn probe(&self) -> impl Future<Output = Vec<ProviderStatus>> + Send + 'static {
        let config = self.config.clone();
        let stub = self.stub.clone();
        let machine = self.machine.clone();
        let ssh_hosts = self.ssh_hosts();
        // The user knows peers by name; the key stands in for an unnamed one.
        let peers = self
            .peers
            .as_ref()
            .filter(|link| link.running())
            .map(|link| link.peers().into_iter().map(|peer| link.name(&peer).unwrap_or(peer)).collect::<Vec<_>>());
        async move {
            let status = |kind, availability, detail: String| ProviderStatus { kind, availability, detail };
            use Availability::{NotConfigured, Ready, Unavailable};
            let no_stub = || "loa-sandbox-stub was not found next to the app or on PATH (set LOA_SANDBOX_STUB)".to_owned();
            let kvm = machine.kvm();
            let mut statuses = Vec::with_capacity(SandboxKind::ALL.len());

            statuses.push(match &stub {
                Some(_) => status(SandboxKind::Local, Ready, format!(
                    "Runs on this machine without OS isolation; isolated tasks use a {} checkout",
                    isolation::backend_name()
                )),
                None => status(SandboxKind::Local, Unavailable, no_stub()),
            });

            statuses.push(match (machine.which("docker"), &stub) {
                (None, _) => status(SandboxKind::Docker, Unavailable, "Docker is not installed".into()),
                (Some(_), None) => status(SandboxKind::Docker, Unavailable, no_stub()),
                (Some(docker), Some(_)) => match docker_server_version(&docker).await {
                    Ok(version) => status(SandboxKind::Docker, Ready, format!("Docker {version}, default image {}", config.docker_image)),
                    Err(error) => status(SandboxKind::Docker, Unavailable, format!("The Docker daemon is not reachable: {error}")),
                },
            });

            statuses.push(match (machine.which("firecracker"), kvm, &config.firecracker_kernel, &config.firecracker_rootfs) {
                (None, ..) => status(SandboxKind::Firecracker, Unavailable, "Firecracker is not installed".into()),
                (Some(_), false, ..) => status(SandboxKind::Firecracker, Unavailable, format!("{} is not accessible", machine.kvm_device.display())),
                (Some(_), true, Some(kernel), Some(rootfs)) => status(SandboxKind::Firecracker, Ready, format!("Kernel {}, rootfs {}", kernel.display(), rootfs.display())),
                (Some(_), true, ..) => status(SandboxKind::Firecracker, NotConfigured, "Set a guest kernel and a rootfs whose init runs loa-sandbox-stub on ttyS0".into()),
            });

            statuses.push(match (machine.which(QEMU), &config.vm_disk) {
                (None, _) => status(SandboxKind::Vm, Unavailable, format!("{QEMU} is not installed")),
                (Some(_), None) => status(SandboxKind::Vm, NotConfigured, "Set a guest disk image whose init runs loa-sandbox-stub on the first serial port".into()),
                (Some(_), Some(disk)) => status(SandboxKind::Vm, Ready, format!("Disk {}, {}", disk.display(), if kvm { "KVM accelerated" } else { "without KVM (slow)" })),
            });

            statuses.push(match (machine.which("ssh"), &stub, ssh_hosts.as_slice()) {
                (None, ..) => status(SandboxKind::Ssh, Unavailable, "No ssh client is installed".into()),
                (Some(_), None, _) => status(SandboxKind::Ssh, Unavailable, no_stub()),
                (Some(_), Some(_), []) => status(SandboxKind::Ssh, NotConfigured, "Add a host to ~/.ssh/config or the sandbox settings".into()),
                (Some(_), Some(_), hosts) => status(SandboxKind::Ssh, Ready, format!(
                    "{}; hosts must run {} on {}",
                    hosts.join(", "),
                    std::env::consts::OS,
                    std::env::consts::ARCH
                )),
            });

            statuses.push(match peers.as_deref() {
                None => status(SandboxKind::Peer, Unavailable, "Peer networking is not running".into()),
                Some([]) => status(SandboxKind::Peer, NotConfigured, "No peer has granted this device sandbox access".into()),
                Some(peers) => status(SandboxKind::Peer, Ready, peers.join(", ")),
            });
            statuses
        }
    }
}

async fn docker_server_version(docker: &Path) -> Result<String, String> {
    let mut command = tokio::process::Command::new(docker);
    command.args(["version", "--format", "{{.Server.Version}}"]).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output()).await
        .map_err(|_| "no answer within 5 s".to_owned())?
        .map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() && !stdout.is_empty() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(stderr.lines().find(|line| !line.trim().is_empty()).unwrap_or("docker version failed").trim().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::MemoryJournal;

    fn script(path: &Path, body: &str) {
        loa_util::exec::write_executable(path, body).unwrap();
    }

    fn service(dir: &Path, config: SandboxConfig, stub: Option<PathBuf>) -> SandboxService {
        SandboxService::new(config, Arc::new(MemoryJournal::default()))
            .with_stub(stub)
            .with_machine(Some(dir.join("bin").into_os_string()), dir.join("kvm"), Some(dir.join("ssh_config")))
    }

    fn by_kind(statuses: &[ProviderStatus], kind: SandboxKind) -> &ProviderStatus {
        statuses.iter().find(|status| status.kind == kind).unwrap()
    }

    #[test]
    fn reads_concrete_hosts_from_an_ssh_config() {
        let config = "Host *\n  ForwardAgent no\nHost build build-2 !bad\n  HostName 10.0.0.2\nhost=gpu\nMatch host x\nHost build\n";
        assert_eq!(ssh_config_hosts(config), ["build", "build-2", "gpu"]);
    }

    #[test]
    fn settings_round_trip_and_default_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sandbox.json");
        assert_eq!(SandboxConfig::load(&path).unwrap(), SandboxConfig::default());
        let config = SandboxConfig { vm_disk: Some("/vm.img".into()), ssh_hosts: vec!["build".into()], ..SandboxConfig::default() };
        config.save(&path).unwrap();
        assert_eq!(SandboxConfig::load(&path).unwrap(), config);
    }

    #[tokio::test]
    async fn an_empty_machine_reports_each_missing_piece() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let service = service(dir.path(), SandboxConfig::default(), None);
        let statuses = service.probe().await;
        assert_eq!(statuses.iter().map(|status| status.kind).collect::<Vec<_>>(), SandboxKind::ALL);
        assert!(statuses.iter().all(|status| status.availability == Availability::Unavailable), "{statuses:#?}");
        assert!(by_kind(&statuses, SandboxKind::Local).detail.contains("loa-sandbox-stub was not found"));
        assert_eq!(by_kind(&statuses, SandboxKind::Docker).detail, "Docker is not installed");
        assert_eq!(by_kind(&statuses, SandboxKind::Peer).detail, "Peer networking is not running");
        assert!(service.providers().is_empty());
        let template = SessionConfig::default();
        assert!(service.host_tool(Arc::new(crate::tests::NoAdapter), &template).is_none());
    }

    #[tokio::test]
    async fn a_configured_machine_offers_every_provider() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        script(&bin.join("docker"), "#!/bin/sh\necho 27.1.0\n");
        script(&bin.join("firecracker"), "#!/bin/sh\n");
        script(&bin.join(QEMU), "#!/bin/sh\n");
        script(&bin.join("ssh"), "#!/bin/sh\n");
        std::fs::write(dir.path().join("kvm"), "").unwrap();
        std::fs::write(dir.path().join("ssh_config"), "Host build\n").unwrap();
        let stub = dir.path().join("stub");
        script(&stub, "#!/bin/sh\n");
        let config = SandboxConfig {
            firecracker_kernel: Some("/k".into()),
            firecracker_rootfs: Some("/r".into()),
            vm_disk: Some("/d".into()),
            ..SandboxConfig::default()
        };
        struct Peers;
        #[async_trait::async_trait]
        impl PeerLink for Peers {
            fn peers(&self) -> Vec<String> { vec!["studio".into()] }
            async fn open(&self, _: &str) -> crate::Result<crate::Channel> { Err(crate::SandboxError::Disconnected) }
        }
        let service = service(dir.path(), config, Some(stub)).with_peers(Arc::new(Peers));
        let statuses = service.probe().await;
        assert!(statuses.iter().all(|status| status.availability == Availability::Ready), "{statuses:#?}");
        assert_eq!(by_kind(&statuses, SandboxKind::Docker).detail, "Docker 27.1.0, default image debian:stable-slim");
        assert!(by_kind(&statuses, SandboxKind::Ssh).detail.starts_with("build; hosts must run linux"));
        assert_eq!(by_kind(&statuses, SandboxKind::Peer).detail, "studio");
        let mut kinds: Vec<_> = service.providers().iter().map(|provider| provider.kind()).collect();
        kinds.sort();
        assert_eq!(kinds, SandboxKind::ALL);

        let template = SessionConfig { session_id: "parent".into(), cwd: dir.path().into(), ..SessionConfig::default() };
        let (spec, _) = service.host_tool(Arc::new(crate::tests::NoAdapter), &template).unwrap();
        assert_eq!(spec.name, "sandbox_task");
        let schema: serde_json::Value = serde_json::from_str(&spec.input_schema).unwrap();
        assert_eq!(schema["properties"]["kind"]["enum"], serde_json::json!(["local", "docker", "firecracker", "vm", "ssh", "peer"]));
        assert!(spec.description.contains("- ssh: the SSH host build"), "{}", spec.description);
        assert!(spec.description.contains("- peer: a peer node; set options.peer to one of studio"), "{}", spec.description);
    }

    #[tokio::test]
    async fn reports_installed_but_unconfigured_providers() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        script(&bin.join("docker"), "#!/bin/sh\necho 'Cannot connect to the Docker daemon at unix:///var/run/docker.sock.' >&2\nexit 1\n");
        script(&bin.join("firecracker"), "#!/bin/sh\n");
        script(&bin.join(QEMU), "#!/bin/sh\n");
        script(&bin.join("ssh"), "#!/bin/sh\n");
        std::fs::write(dir.path().join("kvm"), "").unwrap();
        let stub = dir.path().join("stub");
        script(&stub, "#!/bin/sh\n");
        let service = service(dir.path(), SandboxConfig::default(), Some(stub));
        let statuses = service.probe().await;
        let docker = by_kind(&statuses, SandboxKind::Docker);
        assert_eq!((docker.availability, docker.detail.as_str()), (Availability::Unavailable, "The Docker daemon is not reachable: Cannot connect to the Docker daemon at unix:///var/run/docker.sock."));
        for kind in [SandboxKind::Firecracker, SandboxKind::Vm, SandboxKind::Ssh] {
            assert_eq!(by_kind(&statuses, kind).availability, Availability::NotConfigured, "{kind:?}");
        }
        let mut kinds: Vec<_> = service.providers().iter().map(|provider| provider.kind()).collect();
        kinds.sort();
        assert_eq!(kinds, [SandboxKind::Local, SandboxKind::Docker, SandboxKind::Ssh]);
    }
}
