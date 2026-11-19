//! Process-backed and socket-backed channels used by the providers.
use super::{Channel, Sandbox, WorkspaceMode};
use crate::Result;
use async_trait::async_trait;
use std::process::Stdio;
use tokio::process::{Child, Command};

pub(super) fn command_channel(mut command: Command, mode: WorkspaceMode) -> Result<ProcessSandbox> {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = command.spawn()?;
    let channel = Channel {
        reader: Box::new(child.stdout.take().expect("piped stdout")),
        writer: Box::new(child.stdin.take().expect("piped stdin")),
    };
    Ok(ProcessSandbox { child, channel: Some(channel), mode })
}

pub(super) struct ProcessSandbox {
    child: Child,
    channel: Option<Channel>,
    mode: WorkspaceMode,
}

#[async_trait]
impl Sandbox for ProcessSandbox {
    fn take_channel(&mut self) -> Option<Channel> { self.channel.take() }
    fn workspace_mode(&self) -> WorkspaceMode { self.mode }
    async fn stop(mut self: Box<Self>) -> Result<()> {
        self.channel.take();
        if self.child.try_wait()?.is_none() { self.child.kill().await?; }
        self.child.wait().await?;
        Ok(())
    }
}
