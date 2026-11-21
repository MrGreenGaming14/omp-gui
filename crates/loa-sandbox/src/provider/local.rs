//! Reference provider: the stub is a child in the host workspace.
use super::serial::command_channel;
use super::{Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

pub struct LocalProvider {
    pub stub: PathBuf,
}

impl LocalProvider {
    pub fn new(stub: impl Into<PathBuf>) -> Self { Self { stub: stub.into() } }
}

impl Default for LocalProvider {
    fn default() -> Self { Self::new(STUB_NAME) }
}

/// File name of the stub binary.
pub const STUB_NAME: &str = "loa-sandbox-stub";

/// Finds the stub: `LOA_SANDBOX_STUB`, then next to the running executable
/// or in one of the directories above it (cargo puts test executables up to
/// four levels below the profile directory that holds the binaries), then
/// `PATH`.
pub fn find_stub() -> Option<PathBuf> {
    const LEVELS: usize = 5;
    let explicit = std::env::var_os("LOA_SANDBOX_STUB").map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    let near = exe.iter().flat_map(|exe| exe.ancestors().skip(1).take(LEVELS).map(|dir| dir.join(STUB_NAME)));
    let path = std::env::var_os("PATH").into_iter().flat_map(|paths| std::env::split_paths(&paths).map(|dir| dir.join(STUB_NAME)).collect::<Vec<_>>());
    explicit.into_iter().chain(near).chain(path).find_map(|candidate| loa_exec::find_executable(&candidate))
}

/// What Windows processes need to start at all, and nothing a secret could
/// hide in: the system locations, with `PATH` limited to the system
/// directories (for `cmd`, `ping` and `taskkill`). Everything else a command
/// sees comes from [`SandboxSpec::env`]. Unix needs nothing.
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
        env.push(("PATH".to_owned(), format!(r"{root}\System32;{root}")));
    }
    env
}

#[async_trait]
impl SandboxProvider for LocalProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Local }
    fn describe(&self) -> String { "this machine, in the workspace or its isolated checkout; no OS level isolation".into() }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let mut cmd = Command::new(&self.stub);
        cmd.arg(&spec.workspace).current_dir(&spec.workspace).env_clear().envs(system_env()).envs(spec.env.iter().cloned());
        Ok(Box::new(command_channel(cmd, WorkspaceMode::Shared)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::SandboxOptions;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn uses_workspace_and_only_specified_environment() {
        let dir = tempfile::tempdir().unwrap();
        // A fixture script needs Bash, found through `PATH`; Windows has no
        // system-wide PATH to fall back on once the environment is cleared.
        let mut env = vec![("SANDBOX_TEST".to_owned(), "yes".to_owned())];
        env.extend(std::env::var("PATH").ok().filter(|_| cfg!(windows)).map(|path| ("PATH".to_owned(), path)));
        let spec = SandboxSpec { name: "test".into(), workspace: dir.path().into(), env, options: SandboxOptions::default() };
        // A script stands in for the stub and reports its environment and cwd.
        // Git Bash's `pwd -W` prints the Windows form of the directory.
        let fake = dir.path().join("stub");
        let pwd = if cfg!(windows) { "pwd -W" } else { "pwd" };
        loa_exec::write_executable(&fake, format!("#!/bin/sh\n{pwd}\nprintf '%s' \"$SANDBOX_TEST\"\n")).unwrap();
        let provider = LocalProvider { stub: loa_exec::resolve(&fake) };
        let mut sandbox = provider.start(&spec).await.unwrap();
        assert_eq!(sandbox.workspace_mode(), WorkspaceMode::Shared);
        let mut channel = sandbox.take_channel().unwrap();
        assert!(sandbox.take_channel().is_none());
        let mut output = String::new();
        channel.reader.read_to_string(&mut output).await.unwrap();
        let (reported, shell_env) = output.split_once('\n').unwrap();
        assert_eq!(shell_env, "yes");
        // Bash prints forward slashes, and Windows may spell the temporary
        // directory's parents in short (8.3) or long form; its own name is
        // unique, so compare that.
        let reported = reported.trim_end().replace('\\', "/");
        let name = dir.path().file_name().unwrap().to_string_lossy().into_owned();
        assert!(reported.ends_with(&format!("/{name}")), "{reported} is not {name}");
        channel.writer.shutdown().await.unwrap();
        sandbox.stop().await.unwrap();
    }
}
