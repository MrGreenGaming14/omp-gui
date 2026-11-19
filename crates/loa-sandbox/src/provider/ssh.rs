//! Runs the stub on an SSH host. Workspace transfer is handled by sync over the channel.
use super::serial::command_channel;
use super::{Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::{Result, SandboxError};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

pub struct SshProvider {
    pub ssh: PathBuf,
    pub scp: PathBuf,
    pub host: String,
    pub port: u16,
    pub local_stub: PathBuf,
    pub remote_root: String,
}

impl Default for SshProvider {
    fn default() -> Self {
        Self { ssh: "ssh".into(), scp: "scp".into(), host: String::new(), port: 22, local_stub: "loa-sandbox-stub".into(), remote_root: "/tmp/loa-sandbox".into() }
    }
}

pub(super) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[async_trait]
impl SandboxProvider for SshProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Ssh }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let host = spec.options.host.as_deref().unwrap_or(&self.host);
        if host.is_empty() || host.starts_with('-') {
            return Err(SandboxError::Provider { provider: "ssh", message: "a valid host is required".into() });
        }
        let root = format!("{}/{}", self.remote_root.trim_end_matches('/'), spec.name);
        let setup = format!("mkdir -p -- {} && chmod 700 -- {}", quote(&root), quote(&root));
        let status = Command::new(&self.ssh).arg("-p").arg(self.port.to_string()).arg("--").arg(host).arg(&setup).status().await?;
        if !status.success() { return Err(SandboxError::Provider { provider: "ssh", message: format!("remote setup failed: {status}") }); }
        let stub = format!("{root}/loa-sandbox-stub");
        let upload = Command::new(&self.scp).arg("-P").arg(self.port.to_string()).arg("--").arg(&self.local_stub)
            .arg(format!("{host}:{}", quote(&stub))).status().await?;
        if !upload.success() {
            let _ = cleanup(&self.ssh, self.port, host, &root).await;
            return Err(SandboxError::Provider { provider: "ssh", message: format!("stub upload failed: {upload}") });
        }
        let command = format!("chmod 700 -- {} && exec {} {}", quote(&stub), quote(&stub), quote(&root));
        let mut cmd = Command::new(&self.ssh);
        cmd.arg("-T").arg("-p").arg(self.port.to_string()).arg("--").arg(host).arg(command);
        let process = match command_channel(cmd, WorkspaceMode::Copied) {
            Ok(process) => process,
            Err(e) => { let _ = cleanup(&self.ssh, self.port, host, &root).await; return Err(e); }
        };
        Ok(Box::new(SshSandbox { process: Some(process), ssh: self.ssh.clone(), port: self.port, host: host.into(), root }))
    }
}

async fn cleanup(ssh: &PathBuf, port: u16, host: &str, root: &str) -> Result<()> {
    let status = Command::new(ssh).arg("-p").arg(port.to_string()).arg("--").arg(host)
        .arg(format!("rm -rf -- {}", quote(root))).status().await?;
    if !status.success() { return Err(SandboxError::Provider { provider: "ssh", message: format!("remote cleanup failed: {status}") }); }
    Ok(())
}

struct SshSandbox {
    process: Option<super::serial::ProcessSandbox>,
    ssh: PathBuf,
    port: u16,
    host: String,
    root: String,
}

impl Drop for SshSandbox {
    fn drop(&mut self) {
        if self.process.is_some() {
            let _ = std::process::Command::new(&self.ssh).arg("-p").arg(self.port.to_string()).arg("--").arg(&self.host)
                .arg(format!("rm -rf -- {}", quote(&self.root))).spawn();
        }
    }
}

#[async_trait]
impl Sandbox for SshSandbox {
    fn take_channel(&mut self) -> Option<super::Channel> { self.process.as_mut().and_then(|p| p.take_channel()) }
    fn workspace_mode(&self) -> WorkspaceMode { WorkspaceMode::Copied }
    async fn stop(mut self: Box<Self>) -> Result<()> {
        if let Some(process) = self.process.take() { Box::new(process).stop().await?; }
        cleanup(&self.ssh, self.port, &self.host, &self.root).await
    }
}

#[cfg(test)]
mod tests {
    use super::quote;
    #[test]
    fn shell_quote_handles_spaces_and_apostrophes() {
        assert_eq!(quote("a b'c"), "'a b'\\''c'");
    }
}
