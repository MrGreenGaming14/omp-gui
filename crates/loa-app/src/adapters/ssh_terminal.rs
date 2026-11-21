//! The terminal's remote PTY port over `loa-remote`'s SSH shells.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use loa_remote::RemoteService;
use loa_remote::pty::{ShellRead, ShellReader, ShellWriter};
use loa_view::terminal::port;

/// [`port::RemotePty`] over the service's SSH shells.
pub struct SshPty(pub Arc<RemoteService>);

#[async_trait]
impl port::RemotePty for SshPty {
    async fn open_pty(&self, host: &str, size: port::GridSize) -> port::Result<port::PtyChannel> {
        let shell = self.0.open_shell(host, size.cols, size.rows).await.map_err(|error| remote_error(host, error))?;
        Ok(port::PtyChannel { reader: Box::new(SshReader(shell.reader)), writer: Box::new(SshWriter(shell.writer)) })
    }
}

fn remote_error(host: &str, error: loa_remote::Error) -> port::Error {
    port::Error::Remote { host: host.to_owned(), message: error.to_string() }
}

struct SshReader(ShellReader);

#[async_trait]
impl port::ChannelReader for SshReader {
    async fn read(&mut self) -> port::Result<port::ChannelRead> {
        Ok(match self.0.read().await {
            ShellRead::Data(data) => port::ChannelRead::Data(data),
            ShellRead::Closed { exit_status } => port::ChannelRead::Closed { exit_code: exit_status },
        })
    }
}

struct SshWriter(ShellWriter);

#[async_trait]
impl port::ChannelWriter for SshWriter {
    async fn write(&mut self, bytes: Bytes) -> port::Result<()> {
        let host = self.0.host().to_owned();
        self.0.write(bytes).await.map_err(|error| remote_error(&host, error))
    }

    async fn resize(&mut self, size: port::GridSize) -> port::Result<()> {
        let host = self.0.host().to_owned();
        self.0.resize(size.cols, size.rows).await.map_err(|error| remote_error(&host, error))
    }

    async fn close(&mut self) -> port::Result<()> {
        let host = self.0.host().to_owned();
        self.0.close().await.map_err(|error| remote_error(&host, error))
    }
}
