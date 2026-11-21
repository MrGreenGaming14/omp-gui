//! The byte stream behind a terminal pane, and the ports that open one: [`PtyBackend`] for
//! any [`Target`], with the [`LocalPtyBackend`] and [`RemotePtyBackend`] adapters, and
//! [`RemotePty`], which the SSH layer implements.

use crate::{state::Target, Error, GridSize, Result, local};
use async_trait::async_trait;
use bytes::Bytes;
use futures::future::BoxFuture;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Output, or the program's end; `exit_code` is `None` when it was killed or the transport didn't say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelRead { Data(Bytes), Closed { exit_code: Option<u32> } }

/// Read half of a PTY channel; not called again after `Closed` or an error.
#[async_trait]
pub trait ChannelReader: Send {
    async fn read(&mut self) -> Result<ChannelRead>;
}

/// Reads queued by another task; a dropped sender reads as [`Error::Closed`].
#[async_trait]
impl ChannelReader for mpsc::Receiver<Result<ChannelRead>> {
    async fn read(&mut self) -> Result<ChannelRead> { self.recv().await.unwrap_or(Err(Error::Closed)) }
}

/// Write half of a PTY channel. Dropping it must hang up like `close`.
#[async_trait]
pub trait ChannelWriter: Send {
    async fn write(&mut self, bytes: Bytes) -> Result<()>;
    async fn resize(&mut self, size: GridSize) -> Result<()>;
    async fn close(&mut self) -> Result<()>;
}

/// An open PTY, split so output is read while input is written.
pub struct PtyChannel { pub reader: Box<dyn ChannelReader>, pub writer: Box<dyn ChannelWriter> }

impl std::fmt::Debug for PtyChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("PtyChannel") }
}

/// Port: an interactive login shell on `host`, a name from the Remote hosts section.
#[async_trait]
pub trait RemotePty: Send + Sync {
    async fn open_pty(&self, host: &str, size: GridSize) -> Result<PtyChannel>;
}

/// Port: opens a PTY for a target. The terminal state holds one and never matches on targets.
pub trait PtyBackend: Send + Sync {
    fn open(&self, target: &Target, size: GridSize) -> BoxFuture<'static, Result<PtyChannel>>;
}

/// Local programs on a `portable_pty`; remote targets fail with [`Error::NoRemote`].
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalPtyBackend;

impl PtyBackend for LocalPtyBackend {
    fn open(&self, target: &Target, size: GridSize) -> BoxFuture<'static, Result<PtyChannel>> {
        let shell = if let Target::Local(shell) = target { Ok(shell.clone()) } else { Err(Error::NoRemote) };
        Box::pin(async move { local::spawn(&shell?, size) })
    }
}

/// Remote targets through a [`RemotePty`]; everything else goes to `fallback`.
pub struct RemotePtyBackend { pub remote: Arc<dyn RemotePty>, pub fallback: Arc<dyn PtyBackend> }

impl PtyBackend for RemotePtyBackend {
    fn open(&self, target: &Target, size: GridSize) -> BoxFuture<'static, Result<PtyChannel>> {
        let Target::Remote { host } = target else { return self.fallback.open(target, size) };
        let (remote, host) = (Arc::clone(&self.remote), host.clone());
        Box::pin(async move { remote.open_pty(&host, size).await })
    }
}
