//! Terminal tab for local shells and SSH hosts: an `alacritty_terminal` emulator on an iced canvas, fed by
//! a [`PtyChannel`] that a [`PtyBackend`] ([`LocalPtyBackend`], [`RemotePtyBackend`]) opens for a [`Target`].
//! The channel ports live in [`loa_view::terminal::port`].

pub mod canvas;
pub mod emulator;
pub mod input;
pub mod local;
pub mod palette;
pub mod state;

pub use local::LocalShell;
pub use state::{Message, State, Status, TabId, Target};
use async_trait::async_trait;
use futures::future::BoxFuture;
use loa_view::terminal::port::{ChannelRead, ChannelReader, Error, GridSize, PtyChannel, RemotePty, Result};
use std::sync::Arc;

/// Reads queued by another task; a dropped sender reads as [`Error::Closed`].
pub(crate) struct QueueReader(pub(crate) tokio::sync::mpsc::Receiver<Result<ChannelRead>>);
#[async_trait]
impl ChannelReader for QueueReader {
    async fn read(&mut self) -> Result<ChannelRead> { self.0.recv().await.unwrap_or(Err(Error::Closed)) }
}

/// Port: opens a PTY for a target. The terminal state holds one and never matches on targets.
pub trait PtyBackend: Send + Sync { fn open(&self, target: &Target, size: GridSize) -> BoxFuture<'static, Result<PtyChannel>>; }

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
