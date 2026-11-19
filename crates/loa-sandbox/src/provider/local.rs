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
    fn default() -> Self { Self::new("loa-sandbox-stub") }
}

#[async_trait]
impl SandboxProvider for LocalProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Local }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let mut cmd = Command::new(&self.stub);
        cmd.arg(&spec.workspace).current_dir(&spec.workspace).env_clear().envs(spec.env.iter().cloned());
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
        let spec = SandboxSpec { name: "test".into(), workspace: dir.path().into(), env: vec![("SANDBOX_TEST".into(), "yes".into())], options: SandboxOptions::default() };
        // A script stands in for the stub and reports its environment and cwd.
        let fake = dir.path().join("stub");
        std::fs::write(&fake, "#!/bin/sh\npwd\nprintf '%s' \"$SANDBOX_TEST\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let provider = LocalProvider { stub: fake };
        let mut sandbox = provider.start(&spec).await.unwrap();
        assert_eq!(sandbox.workspace_mode(), WorkspaceMode::Shared);
        let mut channel = sandbox.take_channel().unwrap();
        assert!(sandbox.take_channel().is_none());
        let mut output = String::new();
        channel.reader.read_to_string(&mut output).await.unwrap();
        assert_eq!(output, format!("{}\nyes", dir.path().display()));
        channel.writer.shutdown().await.unwrap();
        sandbox.stop().await.unwrap();
    }
}
