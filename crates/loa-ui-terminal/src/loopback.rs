//! In-process [`RemotePty`] for tests and demos.
//!
//! Every channel the pane opens shows up as a [`LoopbackSession`], which
//! plays the remote host: it sees what the pane writes, resizes and closes,
//! and sends output and an exit status back.

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::mpsc;

use crate::channel::{ChannelRead, ChannelReader, ChannelWriter, PtyChannel, RemotePty};
use crate::{Error, GridSize, Result};

/// What the pane sent to the remote side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteInput {
    /// Input bytes.
    Data(Bytes),
    /// A new grid size.
    Resize(GridSize),
    /// The pane hung up.
    Close,
}

/// A [`RemotePty`] whose hosts are played by the holder of the session
/// receiver returned from [`LoopbackHost::new`].
#[derive(Debug)]
pub struct LoopbackHost {
    sessions: mpsc::UnboundedSender<LoopbackSession>,
}

impl LoopbackHost {
    /// Creates the port and the receiver that gets one [`LoopbackSession`]
    /// per opened channel. Dropping the receiver makes every later open fail,
    /// like an unreachable host.
    pub fn new() -> (Self, mpsc::UnboundedReceiver<LoopbackSession>) {
        let (sessions, receiver) = mpsc::unbounded_channel();
        (Self { sessions }, receiver)
    }
}

#[async_trait]
impl RemotePty for LoopbackHost {
    async fn open_pty(&self, host: &str, size: GridSize) -> Result<PtyChannel> {
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (output_tx, output_rx) = mpsc::unbounded_channel();
        let session = LoopbackSession {
            host: host.to_owned(),
            size,
            input: input_rx,
            output: output_tx,
        };
        self.sessions.send(session).map_err(|_| Error::Remote {
            host: host.to_owned(),
            message: "connection refused".into(),
        })?;
        Ok(PtyChannel {
            reader: Box::new(LoopbackReader {
                host: host.to_owned(),
                output: output_rx,
            }),
            writer: Box::new(LoopbackWriter {
                host: host.to_owned(),
                input: Some(input_tx),
            }),
        })
    }
}

/// The remote end of one loopback channel.
#[derive(Debug)]
pub struct LoopbackSession {
    /// Host name the pane asked for.
    pub host: String,
    /// Grid size the channel was opened with.
    pub size: GridSize,
    input: mpsc::UnboundedReceiver<RemoteInput>,
    output: mpsc::UnboundedSender<Result<ChannelRead>>,
}

impl LoopbackSession {
    /// Waits for the next thing the pane sent. `None` once the pane's
    /// writer is gone.
    pub async fn recv(&mut self) -> Option<RemoteInput> {
        self.input.recv().await
    }

    /// Sends output to the pane. Returns false when the pane stopped reading.
    pub fn send(&self, bytes: impl Into<Bytes>) -> bool {
        self.output.send(Ok(ChannelRead::Data(bytes.into()))).is_ok()
    }

    /// Ends the remote program with an exit status.
    pub fn exit(self, exit_code: Option<u32>) {
        let _ = self.output.send(Ok(ChannelRead::Closed { exit_code }));
    }

    /// Drops the connection with a transport error.
    pub fn fail(self, message: &str) {
        let _ = self.output.send(Err(Error::Remote {
            host: self.host.clone(),
            message: message.to_owned(),
        }));
    }
}

struct LoopbackReader {
    host: String,
    output: mpsc::UnboundedReceiver<Result<ChannelRead>>,
}

#[async_trait]
impl ChannelReader for LoopbackReader {
    async fn read(&mut self) -> Result<ChannelRead> {
        match self.output.recv().await {
            Some(read) => read,
            None => Err(Error::Remote {
                host: self.host.clone(),
                message: "connection lost".into(),
            }),
        }
    }
}

struct LoopbackWriter {
    host: String,
    input: Option<mpsc::UnboundedSender<RemoteInput>>,
}

impl LoopbackWriter {
    fn send(&self, input: RemoteInput) -> Result<()> {
        let sender = self.input.as_ref().ok_or(Error::Closed)?;
        sender.send(input).map_err(|_| Error::Remote {
            host: self.host.clone(),
            message: "channel closed by remote".into(),
        })
    }
}

#[async_trait]
impl ChannelWriter for LoopbackWriter {
    async fn write(&mut self, bytes: Bytes) -> Result<()> {
        self.send(RemoteInput::Data(bytes))
    }

    async fn resize(&mut self, size: GridSize) -> Result<()> {
        self.send(RemoteInput::Resize(size))
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(sender) = self.input.take() {
            let _ = sender.send(RemoteInput::Close);
        }
        Ok(())
    }
}

impl Drop for LoopbackWriter {
    fn drop(&mut self) {
        if let Some(sender) = self.input.take() {
            let _ = sender.send(RemoteInput::Close);
        }
    }
}
