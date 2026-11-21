//! VM providers (feature `vm`): Firecracker through its API socket (Linux
//! only), and a generic qemu VM. The guest init must run the stub on the
//! first serial port.

use super::{Availability, Machine, ProviderFactory, ProviderStatus, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode, process, status};
use crate::Result;
use crate::service::SandboxConfig;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;

#[cfg(target_os = "linux")]
pub use firecracker::FirecrackerProvider;

const QEMU: &str = if cfg!(target_arch = "aarch64") { "qemu-system-aarch64" } else { "qemu-system-x86_64" };

#[cfg(target_os = "linux")]
mod firecracker {
    use super::super::{Sandbox, SandboxKind, SandboxProvider, SandboxSpec, Teardown, WorkspaceMode, process};
    use crate::{Result, SandboxError};
    use async_trait::async_trait;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;

    pub struct FirecrackerProvider {
        pub firecracker: PathBuf,
        pub kernel: PathBuf,
        pub rootfs: PathBuf,
        pub memory_mib: u32,
        pub cpus: u32,
    }

    fn error(message: impl Into<String>) -> SandboxError {
        SandboxError::Provider { provider: "firecracker", message: message.into() }
    }

    async fn api(socket: &Path, path: &str, body: serde_json::Value) -> Result<()> {
        let mut connection = tokio::net::UnixStream::connect(socket).await?;
        let body = body.to_string();
        let request = format!("PUT {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        connection.write_all(request.as_bytes()).await?;
        let mut response = String::new();
        connection.read_to_string(&mut response).await?;
        match response.starts_with("HTTP/1.1 2") || response.starts_with("HTTP/1.0 2") {
            true => Ok(()),
            false => Err(error(format!("{path}: {response}"))),
        }
    }

    /// Removes the run directory with the API socket and the private rootfs copy.
    struct RemoveDir(PathBuf);

    impl Teardown for RemoveDir {
        fn command(&self) -> std::process::Command {
            let mut cmd = std::process::Command::new("rm");
            cmd.arg("-rf").arg("--").arg(&self.0);
            cmd
        }
    }

    #[async_trait]
    impl SandboxProvider for FirecrackerProvider {
        fn kind(&self) -> SandboxKind { SandboxKind::Firecracker }
        fn describe(&self) -> String { "a Firecracker microVM without network; the workspace is copied in and out".into() }
        async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
            if spec.options.network == Some(true) {
                return Err(error("network access requires a configured guest TAP device"));
            }
            // Runs never change the configured image and leave nothing behind.
            let run_dir = tempfile::Builder::new().prefix("loa-fc-").tempdir()?.keep();
            let (socket, rootfs) = (run_dir.join("api.sock"), run_dir.join("rootfs"));
            let teardown = Box::new(RemoveDir(run_dir));
            tokio::fs::copy(&self.rootfs, &rootfs).await.map_err(|e| error(format!("copying {}: {e}", self.rootfs.display())))?;
            let mut cmd = Command::new(&self.firecracker);
            cmd.arg("--api-sock").arg(&socket);
            let sandbox = process("firecracker", cmd, WorkspaceMode::Copied, Some(teardown))?;
            for _ in 0..100 {
                if socket.exists() { break; }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let cpus = spec.options.cpus.unwrap_or(self.cpus);
            api(&socket, "/machine-config", json!({ "vcpu_count": cpus, "mem_size_mib": spec.options.memory_mib.unwrap_or(self.memory_mib) })).await?;
            api(&socket, "/boot-source", json!({ "kernel_image_path": self.kernel, "boot_args": "console=ttyS0 reboot=k panic=1 pci=off" })).await?;
            api(&socket, "/drives/rootfs", json!({ "drive_id": "rootfs", "path_on_host": rootfs, "is_root_device": true, "is_read_only": false })).await?;
            api(&socket, "/actions", json!({ "action_type": "InstanceStart" })).await?;
            Ok(Box::new(sandbox))
        }
    }
}

pub struct QemuProvider {
    pub qemu: PathBuf,
    /// Raw guest disk. Runs boot it with `-snapshot`, so it never changes.
    pub disk: PathBuf,
    /// Booted with `console=ttyS0 root=/dev/vda rw` when set.
    pub kernel: Option<PathBuf>,
    pub kvm: bool,
    pub memory_mib: u32,
    pub cpus: u32,
}

#[async_trait]
impl SandboxProvider for QemuProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Vm }
    fn describe(&self) -> String { "a qemu virtual machine (options.network to allow network); the workspace is copied in and out".into() }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let mut cmd = Command::new(&self.qemu);
        cmd.args(["-display", "none", "-monitor", "none", "-serial", "stdio", "-snapshot", "-m"])
            .arg(spec.options.memory_mib.unwrap_or(self.memory_mib).to_string())
            .arg("-smp").arg(spec.options.cpus.unwrap_or(self.cpus).to_string())
            .arg("-drive").arg(format!("file={},format=raw,if=virtio", self.disk.display()))
            .args(["-nic", if spec.options.network == Some(true) { "user,model=virtio-net-pci" } else { "none" }]);
        if self.kvm {
            cmd.args(["-accel", "kvm"]);
        }
        if let Some(kernel) = &self.kernel {
            cmd.arg("-kernel").arg(kernel).args(["-append", "console=ttyS0 root=/dev/vda rw"]);
        }
        Ok(Box::new(process("vm", cmd, WorkspaceMode::Copied, None)?))
    }
}

pub struct Firecracker;

#[async_trait]
impl ProviderFactory for Firecracker {
    fn kind(&self) -> SandboxKind { SandboxKind::Firecracker }
    async fn probe(&self, m: &Machine, c: &SandboxConfig) -> ProviderStatus {
        use Availability::*;
        if !cfg!(target_os = "linux") {
            return status(self.kind(), Unavailable, "Firecracker needs Linux (KVM)");
        }
        match (m.which("firecracker"), m.kvm(), &c.firecracker_kernel, &c.firecracker_rootfs) {
            (None, ..) => status(self.kind(), Unavailable, "Firecracker is not installed"),
            (Some(_), false, ..) => status(self.kind(), Unavailable, "/dev/kvm is not accessible"),
            (Some(_), true, Some(k), Some(r)) => status(self.kind(), Ready, format!("Kernel {}, rootfs {}", k.display(), r.display())),
            _ => status(self.kind(), NotConfigured, "Set a guest kernel and a rootfs whose init runs loa-sandbox-stub on ttyS0"),
        }
    }
    #[cfg(target_os = "linux")]
    fn build(&self, m: &Machine, c: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>> {
        let (kernel, rootfs) = (c.firecracker_kernel.clone()?, c.firecracker_rootfs.clone()?);
        Some(Arc::new(FirecrackerProvider { firecracker: m.which("firecracker")?, kernel, rootfs, memory_mib: 512, cpus: 1 }))
    }
    #[cfg(not(target_os = "linux"))]
    fn build(&self, _: &Machine, _: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>> {
        None
    }
}

pub struct Qemu;

#[async_trait]
impl ProviderFactory for Qemu {
    fn kind(&self) -> SandboxKind { SandboxKind::Vm }
    async fn probe(&self, m: &Machine, c: &SandboxConfig) -> ProviderStatus {
        let speed = if m.kvm() { "KVM accelerated" } else { "without KVM (slow)" };
        match (m.which(QEMU), &c.vm_disk) {
            (None, _) => status(self.kind(), Availability::Unavailable, format!("{QEMU} is not installed")),
            (Some(_), None) => status(self.kind(), Availability::NotConfigured, "Set a guest disk image whose init runs loa-sandbox-stub on the first serial port"),
            (Some(_), Some(disk)) => status(self.kind(), Availability::Ready, format!("Disk {}, {speed}", disk.display())),
        }
    }
    fn build(&self, m: &Machine, c: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>> {
        let (qemu, disk, kernel) = (m.which(QEMU)?, c.vm_disk.clone()?, c.vm_kernel.clone());
        Some(Arc::new(QemuProvider { qemu, disk, kernel, kvm: m.kvm(), memory_mib: 512, cpus: 1 }))
    }
}
