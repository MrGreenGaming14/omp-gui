//! Firecracker API socket boot; the guest init must run the stub on ttyS0.
use super::serial::{command_channel, ProcessSandbox};
use super::{Channel, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::{Result, SandboxError};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::Command;

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

pub struct FirecrackerProvider {
    pub firecracker: PathBuf,
    pub kernel: PathBuf,
    pub rootfs: PathBuf,
    pub memory_mib: u32,
    pub cpus: u32,
    pub network: bool,
}

impl FirecrackerProvider {
    pub fn new(kernel: impl Into<PathBuf>, rootfs: impl Into<PathBuf>) -> Self {
        Self { firecracker: "firecracker".into(), kernel: kernel.into(), rootfs: rootfs.into(), memory_mib: 512, cpus: 1, network: false }
    }
}

fn error(message: impl Into<String>) -> SandboxError {
    SandboxError::Provider { provider: "firecracker", message: message.into() }
}

async fn api(socket: &PathBuf, path: &str, body: &str) -> Result<()> {
    let mut connection = UnixStream::connect(socket).await?;
    let request = format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    connection.write_all(request.as_bytes()).await?;
    let mut response = Vec::new();
    connection.read_to_end(&mut response).await?;
    let first = response.split(|&b| b == b'\n').next().unwrap_or_default();
    let status = String::from_utf8_lossy(first);
    if !status.starts_with("HTTP/1.1 2") && !status.starts_with("HTTP/1.0 2") {
        return Err(error(format!("{path}: {}", String::from_utf8_lossy(&response))));
    }
    Ok(())
}

#[async_trait]
impl SandboxProvider for FirecrackerProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Firecracker }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        if spec.options.network.unwrap_or(self.network) {
            return Err(error("network access requires a configured guest TAP device"));
        }
        let socket = std::env::temp_dir().join(format!("loa-fc-{}-{}.sock", std::process::id(), NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)));
        let mut cmd = Command::new(&self.firecracker);
        cmd.arg("--api-sock").arg(&socket);
        let process = command_channel(cmd, WorkspaceMode::Copied)?;
        let configured = async {
            for _ in 0..100 {
                if socket.exists() { break; }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            if !socket.exists() { return Err(error("API socket did not appear")); }
            api(&socket, "/machine-config", &serde_json::json!({
                "vcpu_count": spec.options.cpus.unwrap_or(self.cpus),
                "mem_size_mib": spec.options.memory_mib.unwrap_or(self.memory_mib),
            }).to_string()).await?;
            api(&socket, "/boot-source", &serde_json::json!({
                "kernel_image_path": self.kernel,
                "boot_args": "console=ttyS0 reboot=k panic=1 pci=off",
            }).to_string()).await?;
            api(&socket, "/drives/rootfs", &serde_json::json!({
                "drive_id": "rootfs", "path_on_host": self.rootfs,
                "is_root_device": true, "is_read_only": false,
            }).to_string()).await?;
            api(&socket, "/actions", &serde_json::json!({ "action_type": "InstanceStart" }).to_string()).await
        }.await;
        if let Err(e) = configured {
            let _ = Box::new(process).stop().await;
            let _ = std::fs::remove_file(&socket);
            return Err(e);
        }
        Ok(Box::new(FirecrackerSandbox { process: Some(process), socket }))
    }
}

struct FirecrackerSandbox { process: Option<ProcessSandbox>, socket: PathBuf }

impl Drop for FirecrackerSandbox {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.socket); }
}

#[async_trait]
impl Sandbox for FirecrackerSandbox {
    fn take_channel(&mut self) -> Option<Channel> { self.process.as_mut().and_then(|p| p.take_channel()) }
    fn workspace_mode(&self) -> WorkspaceMode { WorkspaceMode::Copied }
    async fn stop(mut self: Box<Self>) -> Result<()> {
        if let Some(process) = self.process.take() { Box::new(process).stop().await?; }
        Ok(())
    }
}
