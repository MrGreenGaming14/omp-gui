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
use bytes::{Bytes, BytesMut};
use futures::future::BoxFuture;
use loa_view::terminal::port::{ChannelRead, ChannelReader, Error, GridSize, PtyChannel, RemotePty, Result};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Output joined into one read. Gathering stops once a burst reaches this size, so a flood still reaches the
/// pane in bounded pieces.
const BURST_LIMIT: usize = 256 * 1024;

/// Reads queued by another task; a dropped sender reads as [`Error::Closed`]. Output already queued when the pane
/// reads is joined into one chunk (up to [`BURST_LIMIT`]), so a burst takes a single read.
pub(crate) struct QueueReader {
    queue: mpsc::Receiver<Result<ChannelRead>>,
    /// What ended the last burst (an error or the close), returned by the next read.
    pending: Option<Result<ChannelRead>>,
}
impl QueueReader {
    pub(crate) fn new(queue: mpsc::Receiver<Result<ChannelRead>>) -> Self { Self { queue, pending: None } }

    /// Joins `first` with the output chunks already queued.
    fn coalesce(&mut self, first: Bytes) -> Bytes {
        let mut rest = Vec::new();
        let mut len = first.len();
        while len < BURST_LIMIT {
            match self.queue.try_recv() {
                Ok(Ok(ChannelRead::Data(bytes))) => {
                    len += bytes.len();
                    rest.push(bytes);
                }
                Ok(other) => {
                    self.pending = Some(other);
                    break;
                }
                // Empty or closed: the next `recv` says which.
                Err(_) => break,
            }
        }
        if rest.is_empty() { return first }
        let mut burst = BytesMut::with_capacity(len);
        burst.extend_from_slice(&first);
        rest.iter().for_each(|chunk| burst.extend_from_slice(chunk));
        burst.freeze()
    }
}
#[async_trait]
impl ChannelReader for QueueReader {
    async fn read(&mut self) -> Result<ChannelRead> {
        let next = match self.pending.take() { Some(next) => next, None => self.queue.recv().await.unwrap_or(Err(Error::Closed)) };
        match next {
            Ok(ChannelRead::Data(bytes)) => Ok(ChannelRead::Data(self.coalesce(bytes))),
            other => other,
        }
    }
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
