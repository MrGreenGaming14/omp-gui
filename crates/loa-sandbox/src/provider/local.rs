//! Reference provider: the stub is a child in the host workspace.

use super::{Availability, Machine, ProviderFactory, ProviderStatus, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode, process, status};
use crate::Result;
use crate::service::SandboxConfig;
use async_trait::async_trait;
use loa_util::exec::is_executable;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;

/// File name of the stub binary.
pub const STUB_NAME: &str = "loa-sandbox-stub";

/// Finds the stub: `LOA_SANDBOX_STUB`, then next to the running executable
/// or up to five directories above it (where cargo puts test executables),
/// then `PATH`.
pub fn find_stub() -> Option<PathBuf> {
    let explicit = std::env::var_os("LOA_SANDBOX_STUB").map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    let near = exe.iter().flat_map(|exe| exe.ancestors().skip(1).take(5).map(|dir| dir.join(STUB_NAME)));
    explicit.into_iter().chain(near).chain(loa_util::exec::which(STUB_NAME)).find(|candidate| is_executable(candidate))
}

pub struct LocalProvider {
    pub stub: PathBuf,
}

impl LocalProvider {
    pub fn new(stub: impl Into<PathBuf>) -> Self { Self { stub: stub.into() } }
}

#[async_trait]
impl SandboxProvider for LocalProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Local }
    fn describe(&self) -> String { "this machine, in the workspace or its isolated checkout; no OS level isolation".into() }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let mut cmd = Command::new(&self.stub);
        cmd.arg(&spec.workspace).current_dir(&spec.workspace).env_clear().envs(spec.env.iter().cloned());
        Ok(Box::new(process("local", cmd, WorkspaceMode::Shared, None)?))
    }
}

pub struct Factory;

#[async_trait]
impl ProviderFactory for Factory {
    fn kind(&self) -> SandboxKind { SandboxKind::Local }
    async fn probe(&self, machine: &Machine, _: &SandboxConfig) -> ProviderStatus {
        match machine.stub {
            Some(_) => status(self.kind(), Availability::Ready, format!(
                "Runs on this machine without OS isolation; isolated tasks use a {} checkout",
                crate::isolation::backend_name()
            )),
            None => status(self.kind(), Availability::Unavailable, super::NO_STUB),
        }
    }
    fn build(&self, machine: &Machine, _: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>> {
        Some(Arc::new(LocalProvider::new(machine.stub.as_ref()?)))
    }
}
