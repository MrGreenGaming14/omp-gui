//! The terminal tab's ports: PTY channels the app opens on remote hosts.

/// Ports the terminal needs and the app implements.
pub mod port {
    use async_trait::async_trait;
    use bytes::Bytes;

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
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct GridSize {
        pub cols: u16,
        pub rows: u16,
        pub cell_width: u16,
        pub cell_height: u16,
    }

    impl GridSize {
        pub const fn new(cols: u16, rows: u16) -> Self {
            Self { cols, rows, cell_width: 0, cell_height: 0 }
        }
    }

    impl Default for GridSize {
        fn default() -> Self {
            Self::new(80, 24)
        }
    }

    /// Output, or the program's end; `exit_code` is `None` when it was killed or the transport didn't say.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum ChannelRead {
        Data(Bytes),
        Closed { exit_code: Option<u32> },
    }

    /// Read half of a PTY channel; not called again after `Closed` or an error.
    #[async_trait]
    pub trait ChannelReader: Send {
        async fn read(&mut self) -> Result<ChannelRead>;
    }

    /// Write half of a PTY channel. Dropping it must hang up like `close`.
    #[async_trait]
    pub trait ChannelWriter: Send {
        async fn write(&mut self, bytes: Bytes) -> Result<()>;
        async fn resize(&mut self, size: GridSize) -> Result<()>;
        async fn close(&mut self) -> Result<()>;
    }

    /// An open PTY, split so output is read while input is written.
    pub struct PtyChannel {
        pub reader: Box<dyn ChannelReader>,
        pub writer: Box<dyn ChannelWriter>,
    }

    impl std::fmt::Debug for PtyChannel {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("PtyChannel")
        }
    }

    /// An interactive login shell on `host`, a name from the Remote hosts section.
    #[async_trait]
    pub trait RemotePty: Send + Sync {
        async fn open_pty(&self, host: &str, size: GridSize) -> Result<PtyChannel>;
    }
}
