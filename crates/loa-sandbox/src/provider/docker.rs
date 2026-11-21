//! Docker CLI provider; mounts the host workspace and keeps the stub on stdio.

use super::{Availability, Machine, ProviderFactory, ProviderStatus, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, Teardown, WorkspaceMode, process, status};
use crate::Result;
use crate::service::SandboxConfig;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Where the host stub is mounted, read only, inside the container. Works
/// with any image whose C library matches the host's.
const MOUNTED_STUB: &str = "/opt/loa/loa-sandbox-stub";

/// Variables of the host the Docker CLI itself needs to find its daemon and
/// configuration. They reach the `docker` client process only, never the container.
const CLIENT_ENV: &[&str] = &[
    "PATH", "HOME", "XDG_RUNTIME_DIR", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY",
    // Windows: where processes find the system, and the user profile that holds `.docker`.
    "SystemRoot", "windir", "ComSpec", "PATHEXT", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "ProgramData", "ProgramFiles",
];

pub struct DockerProvider {
    pub docker: PathBuf,
    pub stub: PathBuf,
    /// Used when the call names no image.
    pub image: String,
    pub network: bool,
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
        cmd.arg("--mount").arg(format!("type=bind,src={},dst={MOUNTED_STUB},readonly", self.stub.display()));
        if let Some(memory) = spec.options.memory_mib {
            cmd.arg("--memory").arg(format!("{memory}m"));
        }
        if let Some(cpus) = spec.options.cpus {
            cmd.arg("--cpus").arg(cpus.to_string());
        }
        for (key, value) in &spec.env {
            cmd.arg("--env").arg(format!("{key}={value}"));
        }
        let image = spec.options.image.as_deref().filter(|image| !image.is_empty()).unwrap_or(&self.image);
        cmd.args([image, MOUNTED_STUB, "/workspace"]);
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
        match (cfg!(windows), machine.which("docker"), &machine.stub) {
            (true, ..) => status(self.kind(), Unavailable, super::REMOTE_STUB_UNSUPPORTED),
            (_, None, _) => status(self.kind(), Unavailable, "Docker is not installed"),
            (_, Some(_), None) => status(self.kind(), Unavailable, super::NO_STUB),
            (_, Some(docker), Some(_)) => match server_version(&docker).await {
                Ok(version) => status(self.kind(), Ready, format!("Docker {version}, default image {}", config.docker_image)),
                Err(error) => status(self.kind(), Unavailable, format!("The Docker daemon is not reachable: {error}")),
            },
        }
    }
    fn build(&self, machine: &Machine, config: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>> {
        let (docker, stub) = (machine.which("docker").filter(|_| !cfg!(windows))?, machine.stub.clone()?);
        Some(Arc::new(DockerProvider { docker, stub, image: config.docker_image.clone(), network: config.docker_network }))
    }
}

async fn server_version(program: &Path) -> Result<String, String> {
    let mut command = tokio::process::Command::from(docker(program));
    command.args(["version", "--format", "{{.Server.Version}}"]);
    let version = tokio::time::timeout(Duration::from_secs(5), loa_util::exec::run_checked(command)).await.map_err(|_| "no answer within 5 s".to_owned())??;
    Ok(version.trim().to_owned())
}
