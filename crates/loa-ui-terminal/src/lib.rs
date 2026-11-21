//! Terminal tab for local shells and SSH hosts: an `alacritty_terminal` emulator drawn on an
//! iced canvas, fed by a [`PtyChannel`] that a [`PtyBackend`] opens for a [`Target`].

pub mod canvas;
pub mod channel;
pub mod emulator;
pub mod input;
pub mod local;
pub mod palette;
pub mod state;

pub use canvas::{CanvasEvent, Metrics};
pub use channel::{ChannelRead, ChannelReader, ChannelWriter, LocalPtyBackend, PtyBackend, PtyChannel, RemotePty, RemotePtyBackend};
pub use emulator::Emulator;
pub use local::LocalShell;
pub use palette::TermPalette;
pub use state::{Event, Message, State, Status, TabId, Target};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("terminal i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("cannot start shell: {0}")]
    Spawn(String),
    #[error("working directory {} is not a directory", .0.display())]
    NotADirectory(std::path::PathBuf),
    #[error("remote host {host}: {message}")]
    Remote { host: String, message: String },
    #[error("no remote transport is configured")]
    NoRemote,
    #[error("terminal channel is closed")]
    Closed,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A terminal grid in cells, plus one cell's pixel size (0 when unknown) for programs that ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl GridSize {
    pub const fn new(cols: u16, rows: u16) -> Self { Self { cols, rows, cell_width: 0, cell_height: 0 } }
}

impl Default for GridSize {
    /// 80 by 24, used until the pane is laid out.
    fn default() -> Self { Self::new(80, 24) }
}
