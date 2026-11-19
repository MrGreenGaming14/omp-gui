//! Boots a guest image whose init launches the stub on its first serial port.
use super::serial::command_channel;
use super::{Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

pub struct QemuProvider {
    pub qemu: PathBuf,
    /// Raw guest disk. Runs boot it with `-snapshot`, so it never changes.
    pub disk: PathBuf,
    pub kernel: Option<PathBuf>,
    /// Kernel command line, used with `kernel`.
    pub append: String,
    /// Hardware acceleration through `/dev/kvm`.
    pub kvm: bool,
    pub network: bool,
    pub memory_mib: u32,
    pub cpus: u32,
}

impl QemuProvider {
    pub fn new(disk: impl Into<PathBuf>) -> Self {
        Self {
            qemu: "qemu-system-x86_64".into(),
            disk: disk.into(),
            kernel: None,
            append: "console=ttyS0 root=/dev/vda rw".into(),
            kvm: super::kvm_available(),
            network: false,
            memory_mib: 512,
            cpus: 1,
        }
    }
}

#[async_trait]
impl SandboxProvider for QemuProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Vm }
    fn describe(&self) -> String { "a qemu virtual machine (options.network to allow network); the workspace is copied in and out".into() }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let mut cmd = Command::new(&self.qemu);
        cmd.arg("-display").arg("none").arg("-monitor").arg("none")
            .arg("-serial").arg("stdio")
            .arg("-m").arg(spec.options.memory_mib.unwrap_or(self.memory_mib).to_string())
            .arg("-smp").arg(spec.options.cpus.unwrap_or(self.cpus).to_string())
            .arg("-drive").arg(format!("file={},format=raw,if=virtio", self.disk.display()))
            .arg("-snapshot");
        if self.kvm {
            cmd.arg("-accel").arg("kvm");
        }
        if let Some(kernel) = &self.kernel {
            cmd.arg("-kernel").arg(kernel).arg("-append").arg(&self.append);
        }
        if spec.options.network.unwrap_or(self.network) {
            cmd.arg("-nic").arg("user,model=virtio-net-pci");
        } else {
            cmd.arg("-nic").arg("none");
        }
        Ok(Box::new(command_channel(cmd, WorkspaceMode::Copied)?))
    }
}
