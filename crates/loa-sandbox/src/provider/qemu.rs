//! Boots a guest image whose init launches the stub on its first serial port.
use super::serial::command_channel;
use super::{Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

pub struct QemuProvider {
    pub qemu: PathBuf,
    pub disk: PathBuf,
    pub kernel: Option<PathBuf>,
    pub network: bool,
    pub memory_mib: u32,
    pub cpus: u32,
}

impl QemuProvider {
    pub fn new(disk: impl Into<PathBuf>) -> Self {
        Self { qemu: "qemu-system-x86_64".into(), disk: disk.into(), kernel: None, network: false, memory_mib: 512, cpus: 1 }
    }
}

#[async_trait]
impl SandboxProvider for QemuProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Vm }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let mut cmd = Command::new(&self.qemu);
        cmd.arg("-display").arg("none").arg("-monitor").arg("none")
            .arg("-serial").arg("stdio")
            .arg("-m").arg(spec.options.memory_mib.unwrap_or(self.memory_mib).to_string())
            .arg("-smp").arg(spec.options.cpus.unwrap_or(self.cpus).to_string())
            .arg("-drive").arg(format!("file={},format=raw,if=virtio", self.disk.display()));
        if let Some(kernel) = &self.kernel {
            cmd.arg("-kernel").arg(kernel).arg("-append").arg("console=ttyS0");
        }
        if spec.options.network.unwrap_or(self.network) {
            cmd.arg("-nic").arg("user,model=virtio-net-pci");
        } else {
            cmd.arg("-nic").arg("none");
        }
        Ok(Box::new(command_channel(cmd, WorkspaceMode::Copied)?))
    }
}
