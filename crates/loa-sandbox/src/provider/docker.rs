//! Docker CLI provider; mounts the host workspace and keeps the stub on stdio.
use super::serial::command_channel;
use super::{Channel, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::{Result, SandboxError};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

pub struct DockerProvider {
    pub docker: PathBuf,
    pub stub: String,
    pub image: String,
    pub network: bool,
    pub memory_mib: Option<u32>,
    pub cpus: Option<u32>,
}

impl DockerProvider {
    pub fn new(image: impl Into<String>, stub: impl Into<String>) -> Self {
        Self { image: image.into(), stub: stub.into(), ..Self::default() }
    }
}

impl Default for DockerProvider {
    fn default() -> Self {
        Self { docker: "docker".into(), stub: "/usr/local/bin/loa-sandbox-stub".into(), image: "loa-sandbox:latest".into(), network: false, memory_mib: None, cpus: None }
    }
}

#[async_trait]
impl SandboxProvider for DockerProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Docker }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let mut cmd = Command::new(&self.docker);
        cmd.env_clear().arg("run").arg("--rm").arg("-i").arg("--name").arg(&spec.name)
            .arg("--network").arg(if spec.options.network.unwrap_or(self.network) { "bridge" } else { "none" })
            .arg("--mount").arg(format!("type=bind,src={},dst=/workspace", spec.workspace.display()))
            .arg("--workdir").arg("/workspace");
        if let Some(memory) = spec.options.memory_mib.or(self.memory_mib) {
            cmd.arg("--memory").arg(format!("{memory}m"));
        }
        if let Some(cpus) = spec.options.cpus.or(self.cpus) {
            cmd.arg("--cpus").arg(cpus.to_string());
        }
        for (key, value) in &spec.env { cmd.arg("--env").arg(format!("{key}={value}")); }
        cmd.arg(spec.options.image.as_deref().unwrap_or(&self.image)).arg(&self.stub).arg("/workspace");
        let process = command_channel(cmd, WorkspaceMode::Shared)?;
        Ok(Box::new(DockerSandbox { process: Some(process), docker: self.docker.clone(), name: spec.name.clone() }))
    }
}

struct DockerSandbox {
    process: Option<super::serial::ProcessSandbox>,
    docker: PathBuf,
    name: String,
}

impl Drop for DockerSandbox {
    fn drop(&mut self) {
        if self.process.is_some() {
            let _ = std::process::Command::new(&self.docker).args(["rm", "-f", &self.name]).spawn();
        }
    }
}

#[async_trait]
impl Sandbox for DockerSandbox {
    fn take_channel(&mut self) -> Option<Channel> { self.process.as_mut().and_then(Sandbox::take_channel) }
    fn workspace_mode(&self) -> WorkspaceMode { WorkspaceMode::Shared }
    async fn stop(mut self: Box<Self>) -> Result<()> {
        let status = Command::new(&self.docker).args(["rm", "-f", &self.name]).output().await?;
        if !status.status.success() {
            return Err(SandboxError::Provider { provider: "docker", message: String::from_utf8_lossy(&status.stderr).into_owned() });
        }
        if let Some(process) = self.process.take() { Box::new(process).stop().await?; }
        Ok(())
    }
}
