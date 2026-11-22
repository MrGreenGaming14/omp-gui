//! Reference provider: the stub is a child in the host workspace.

use loa_util::exec::NoWindow;
use super::{Availability, Machine, ProviderFactory, ProviderStatus, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode, process, status};
use crate::Result;
use crate::service::SandboxConfig;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::process::Command;

/// File name of the stub binary.
pub const STUB_NAME: &str = "loa-sandbox-stub";

/// Finds the stub: `LOA_SANDBOX_STUB`, then next to the running executable
/// or up to five directories above it (where cargo puts test executables),
/// then `PATH`. Each candidate is resolved like a launch (`.exe` on Windows).
pub fn find_stub() -> Option<PathBuf> {
    let explicit = std::env::var_os("LOA_SANDBOX_STUB").map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    let near = exe.iter().flat_map(|exe| exe.ancestors().skip(1).take(5).map(|dir| dir.join(STUB_NAME)));
    explicit.into_iter().chain(near).find_map(loa_util::exec::which).or_else(|| loa_util::exec::which(STUB_NAME))
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
        cmd.no_window();
        cmd.arg(&spec.workspace).current_dir(&spec.workspace).env_clear().envs(system_env()).envs(spec.env.iter().cloned());
        Ok(Box::new(process("local", cmd, WorkspaceMode::Shared, None)?))
    }
}

/// What Windows processes need to start at all, and nothing a secret could
/// hide in: the system locations, with `PATH` limited to the system
/// directories (for `cmd`, `tar` and `taskkill`) and the directory of the
/// host's POSIX `sh` (Git for Windows), which runs the sandbox file tools.
/// Everything else a command sees comes from [`SandboxSpec::env`]. Unix
/// needs nothing.
pub(super) fn system_env() -> Vec<(String, String)> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let mut env = Vec::new();
    for key in ["SystemRoot", "windir", "ComSpec", "PATHEXT"] {
        if let Ok(value) = std::env::var(key) {
            env.push((key.to_owned(), value));
        }
    }
    if let Ok(root) = std::env::var("SystemRoot") {
        let sh = loa_util::exec::which("sh").and_then(|sh| sh.parent().map(|dir| format!(";{}", dir.display())));
        env.push(("PATH".to_owned(), format!(r"{root}\System32;{root}{}", sh.unwrap_or_default())));
    }
    env
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
