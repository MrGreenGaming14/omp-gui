//! Terminal tab for local shells and SSH hosts.
//!
//! The pane renders an `alacritty_terminal` emulator on an iced canvas. Its
//! bytes come from a [`PtyChannel`]: either a local PTY spawned by
//! [`local::spawn`] or a remote PTY opened through the [`RemotePty`] port,
//! which the SSH layer implements. Several terminals live side by side as sub
//! tabs inside one [`State`].

pub mod canvas;
pub mod channel;
pub mod emulator;
pub mod input;
pub mod local;
pub mod loopback;
pub mod palette;
pub mod state;

pub use canvas::{CanvasEvent, Metrics};
pub use channel::{ChannelRead, ChannelReader, ChannelWriter, PtyChannel, RemotePty};
pub use emulator::Emulator;
pub use local::LocalShell;
pub use loopback::{LoopbackHost, LoopbackSession, RemoteInput};
pub use palette::TermPalette;
pub use state::{Event, Message, State, Status, TabId, Target};
use std::path::PathBuf;

/// Errors raised while opening or driving a terminal channel.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading from or writing to a PTY failed.
    #[error("terminal i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The PTY could not be created or the shell could not be started.
    #[error("cannot start shell: {0}")]
    Spawn(String),
    /// The requested working directory is not a directory.
    #[error("working directory {} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    /// The remote side refused or dropped the channel.
    #[error("remote host {host}: {message}")]
    Remote {
        /// Host the channel was opened on.
        host: String,
        /// What went wrong, as reported by the transport.
        message: String,
    },
    /// A remote target was requested but no [`RemotePty`] was configured.
    #[error("no remote transport is configured")]
    NoRemote,
    /// The channel was already closed.
    #[error("terminal channel is closed")]
    Closed,
}

/// Result alias for this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Size of a terminal grid in character cells, plus the pixel size of one
/// cell (0 when unknown) for programs that ask for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    /// Number of columns.
    pub cols: u16,
    /// Number of rows.
    pub rows: u16,
    /// Width of one cell in pixels.
    pub cell_width: u16,
    /// Height of one cell in pixels.
    pub cell_height: u16,
}

impl GridSize {
    /// A grid of `cols` by `rows` with unknown pixel cell size.
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cell_width: 0,
            cell_height: 0,
        }
    }
}

impl Default for GridSize {
    /// The classic 80 by 24 terminal, used until the pane is laid out.
    fn default() -> Self {
        Self::new(80, 24)
    }
}
