//! The byte stream behind a terminal pane, and the port the SSH layer
//! implements to give the pane a remote PTY.

use async_trait::async_trait;
use bytes::Bytes;

use crate::{GridSize, Result};

/// One read from a PTY channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelRead {
    /// Output produced by the program on the other side.
    Data(Bytes),
    /// The program ended and no more output follows. `exit_code` is `None`
    /// when the program was killed by a signal or the transport did not say.
    Closed {
        /// Exit status of the program, when known.
        exit_code: Option<u32>,
    },
}

/// Read half of a PTY channel.
#[async_trait]
pub trait ChannelReader: Send {
    /// Waits for the next chunk of output. After [`ChannelRead::Closed`] or an
    /// error the reader is finished and is not called again.
    async fn read(&mut self) -> Result<ChannelRead>;
}

/// Write half of a PTY channel.
#[async_trait]
pub trait ChannelWriter: Send {
    /// Sends keyboard input (or any bytes) to the program.
    async fn write(&mut self, bytes: Bytes) -> Result<()>;
    /// Tells the program the terminal grid changed size.
    async fn resize(&mut self, size: GridSize) -> Result<()>;
    /// Ends the session: the program is hung up on and the channel released.
    /// Dropping the writer without calling `close` must have the same effect.
    async fn close(&mut self) -> Result<()>;
}

/// An open PTY channel, split so output can be read while input is written.
pub struct PtyChannel {
    /// Output side.
    pub reader: Box<dyn ChannelReader>,
    /// Input and control side.
    pub writer: Box<dyn ChannelWriter>,
}

impl std::fmt::Debug for PtyChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PtyChannel")
    }
}

/// Port: opens an interactive PTY on a host that does not run the app.
///
/// Implemented by the SSH layer (`russh`, keys through `ssh-agent`, host keys
/// checked against `known_hosts`). The terminal pane only sees bytes.
#[async_trait]
pub trait RemotePty: Send + Sync {
    /// Opens a PTY channel running the login shell on `host` with the given
    /// initial size. `host` is the name the user picked in the Remote hosts
    /// section (an `~/.ssh/config` alias or a hand added host).
    async fn open_pty(&self, host: &str, size: GridSize) -> Result<PtyChannel>;
}
