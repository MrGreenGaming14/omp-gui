//! Docker CLI provider; mounts the host workspace and keeps the stub on stdio.

use super::{Availability, Machine, ProviderFactory, ProviderStatus, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, Teardown, WorkspaceMode, process, status};
use crate::Result;
use crate::service::SandboxConfig;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Where a mounted host stub appears inside the container.
pub const MOUNTED_STUB: &str = "/opt/loa/loa-sandbox-stub";

/// Variables of the host the Docker CLI itself needs to find its daemon and
/// configuration. They reach the `docker` client process only, never the container.
const CLIENT_ENV: &[&str] = &["PATH", "HOME", "XDG_RUNTIME_DIR", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY"];

/// How the container gets the stub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockerStub {
    /// Bind mount this host binary read only at [`MOUNTED_STUB`]. Works with
    /// any image whose C library matches the host's.
    Mount(PathBuf),
    /// The image ships the stub at this path.
    InImage(String),
}

pub struct DockerProvider {
    pub docker: PathBuf,
    pub stub: DockerStub,
    /// Used when the call names no image.
    pub image: String,
    pub network: bool,
    pub memory_mib: Option<u32>,
    pub cpus: Option<u32>,
}

impl DockerProvider {
    pub fn new(image: impl Into<String>, stub: DockerStub) -> Self {
        Self { docker: "docker".into(), stub, image: image.into(), network: false, memory_mib: None, cpus: None }
    }
}

/// The `docker` client with only [`CLIENT_ENV`] from the host.
fn docker(program: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.env_clear().envs(CLIENT_ENV.iter().filter_map(|key| std::env::var_os(key).map(|value| (key, value))));
    cmd
}

/// Removes the container by name.
struct Remove { docker: PathBuf, name: String }

impl Teardown for Remove {
    fn command(&self) -> std::process::Command {
        let mut cmd = docker(&self.docker);
        cmd.args(["rm", "-f", &self.name]);
        cmd
    }
    // `--rm` may have removed it already when the stub exited.
    fn gone(&self, stderr: &str) -> bool { stderr.contains("No such container") }
}

#[async_trait]
impl SandboxProvider for DockerProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Docker }
    fn describe(&self) -> String {
        format!("a Docker container (options.image, default {}; network {} unless options.network)", self.image, if self.network { "on" } else { "off" })
    }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let mut cmd = docker(&self.docker);
        let network = if spec.options.network.unwrap_or(self.network) { "bridge" } else { "none" };
        cmd.args(["run", "--rm", "-i", "--name", &spec.name, "--network", network, "--mount"])
            .arg(format!("type=bind,src={},dst=/workspace", spec.workspace.display()))
            .args(["--workdir", "/workspace"]);
        let stub = match &self.stub {
            DockerStub::Mount(host) => {
                cmd.arg("--mount").arg(format!("type=bind,src={},dst={MOUNTED_STUB},readonly", host.display()));
                MOUNTED_STUB
            }
            DockerStub::InImage(path) => path.as_str(),
        };
        if let Some(memory) = spec.options.memory_mib.or(self.memory_mib) {
            cmd.arg("--memory").arg(format!("{memory}m"));
        }
        if let Some(cpus) = spec.options.cpus.or(self.cpus) {
            cmd.arg("--cpus").arg(cpus.to_string());
        }
        for (key, value) in &spec.env {
            cmd.arg("--env").arg(format!("{key}={value}"));
        }
        let image = spec.options.image.as_deref().filter(|image| !image.is_empty()).unwrap_or(&self.image);
        cmd.args([image, stub, "/workspace"]);
        let remove = Remove { docker: self.docker.clone(), name: spec.name.clone() };
        Ok(Box::new(process("docker", cmd.into(), WorkspaceMode::Shared, Some(Box::new(remove)))?))
    }
}

pub struct Factory;

#[async_trait]
impl ProviderFactory for Factory {
    fn kind(&self) -> SandboxKind { SandboxKind::Docker }
    async fn probe(&self, machine: &Machine, config: &SandboxConfig) -> ProviderStatus {
        use Availability::*;
        match (machine.which("docker"), &machine.stub) {
            (None, _) => status(self.kind(), Unavailable, "Docker is not installed"),
            (Some(_), None) => status(self.kind(), Unavailable, super::NO_STUB),
            (Some(docker), Some(_)) => match server_version(&docker).await {
                Ok(version) => status(self.kind(), Ready, format!("Docker {version}, default image {}", config.docker_image)),
                Err(error) => status(self.kind(), Unavailable, format!("The Docker daemon is not reachable: {error}")),
            },
        }
    }
    fn build(&self, machine: &Machine, config: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>> {
        let mut provider = DockerProvider::new(&config.docker_image, DockerStub::Mount(machine.stub.clone()?));
        provider.docker = machine.which("docker")?;
        provider.network = config.docker_network;
        Some(Arc::new(provider))
    }
}

async fn server_version(program: &Path) -> Result<String, String> {
    let mut command = tokio::process::Command::from(docker(program));
    command.args(["version", "--format", "{{.Server.Version}}"]).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output()).await
        .map_err(|_| "no answer within 5 s".to_owned())?
        .map_err(|error| error.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if output.status.success() && !stdout.is_empty() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(stderr.lines().find(|line| !line.trim().is_empty()).unwrap_or("docker version failed").trim().to_owned())
}
