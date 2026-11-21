//! The transport port: what one `omp/surface/1` connection must offer, and
//! an in-memory implementation for tests and same process surfaces.
//!
//! A QUIC connection gives both channels: one bidirectional stream (ordered,
//! reliable) and unreliable datagrams with a size limit.

use async_trait::async_trait;
use bytes::{Buf, Bytes, BytesMut};
use prost::Message;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, mpsc};

use crate::proto::wire::StreamMessage;
use crate::proto::{ProtoError, Result};

/// Why a datagram could not be sent or received.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DatagramError {
    #[error("datagram of {len} bytes exceeds the {max} byte limit")]
    TooLarge { len: usize, max: usize },
    #[error("the connection does not carry datagrams")]
    Unsupported,
    #[error("the connection is closed")]
    Closed,
}

/// Unreliable, unordered datagrams of one connection.
#[async_trait]
pub trait Datagrams: Send + Sync {
    /// Largest datagram `send` accepts right now; `None` without datagrams (TCP fallback).
    fn max_size(&self) -> Option<usize>;

    /// Queues one datagram. A full send buffer drops datagrams rather than
    /// blocking; that is not an error.
    fn send(&self, datagram: Bytes) -> Result<(), DatagramError>;

    /// The next datagram. Must be cancel safe: dropping the future loses no datagram.
    async fn recv(&self) -> Result<Bytes, DatagramError>;
}

pub type BoxReader = Box<dyn AsyncRead + Send + Unpin>;
pub type BoxWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// One `omp/surface/1` connection: the two halves of its bidirectional
/// stream and its datagrams.
pub struct Transport {
    pub reader: BoxReader,
    pub writer: BoxWriter,
    pub datagrams: Arc<dyn Datagrams>,
}

/// Two connected in-memory transports. `max_datagram` of `None` behaves like
/// the TCP fallback: no datagrams.
pub fn memory_pair(max_datagram: Option<usize>) -> (Transport, Transport) {
    let end = |stream: tokio::io::DuplexStream, tx, rx| {
        let (read, write) = tokio::io::split(stream);
        let datagrams = Arc::new(MemoryDatagrams { max: max_datagram, tx, rx: Mutex::new(rx) });
        Transport { reader: Box::new(read), writer: Box::new(write), datagrams }
    };
    let (a, b) = tokio::io::duplex(256 * 1024);
    // Datagrams queued per direction before new ones drop.
    let ((a_tx, b_rx), (b_tx, a_rx)) = (mpsc::channel(1024), mpsc::channel(1024));
    (end(a, a_tx, a_rx), end(b, b_tx, b_rx))
}

/// One end of the datagram channel of [`memory_pair`].
struct MemoryDatagrams {
    max: Option<usize>,
    tx: mpsc::Sender<Bytes>,
    rx: Mutex<mpsc::Receiver<Bytes>>,
}

#[async_trait]
impl Datagrams for MemoryDatagrams {
    fn max_size(&self) -> Option<usize> {
        self.max
    }

    fn send(&self, datagram: Bytes) -> Result<(), DatagramError> {
        let max = self.max.ok_or(DatagramError::Unsupported)?;
        if datagram.len() > max {
            return Err(DatagramError::TooLarge { len: datagram.len(), max });
        }
        match self.tx.try_send(datagram) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(DatagramError::Closed),
        }
    }

    async fn recv(&self) -> Result<Bytes, DatagramError> {
        self.max.ok_or(DatagramError::Unsupported)?;
        self.rx.lock().await.recv().await.ok_or(DatagramError::Closed)
    }
}

/// Largest stream message accepted: a whole frame on the TCP fallback.
const MAX_STREAM_MESSAGE: usize = 64 * 1024 * 1024;

/// Reads length delimited [`StreamMessage`]s. `next` is cancel safe.
pub(crate) struct MessageReader {
    inner: BoxReader,
    buf: BytesMut,
}

impl MessageReader {
    pub fn new(inner: BoxReader) -> Self {
        Self { inner, buf: BytesMut::new() }
    }

    /// The next message, `None` at a clean end of stream.
    pub async fn next(&mut self) -> Result<Option<StreamMessage>> {
        loop {
            let mut wanted = 8 * 1024;
            if let Some(last) = self.buf.iter().take(10).position(|byte| byte & 0x80 == 0) {
                let len = prost::decode_length_delimiter(&self.buf[..])?;
                if len > MAX_STREAM_MESSAGE {
                    return Err(ProtoError::TooLarge { len: len as u64, max: MAX_STREAM_MESSAGE as u64 });
                }
                let prefix = last + 1;
                if self.buf.len() >= prefix + len {
                    self.buf.advance(prefix);
                    return Ok(Some(StreamMessage::decode(self.buf.split_to(len).freeze())?));
                }
                wanted = wanted.max(prefix + len - self.buf.len());
            } else if self.buf.len() >= 10 {
                return Err(ProtoError::Protocol("message length prefix is not a varint".into()));
            }
            self.buf.reserve(wanted);
            if self.inner.read_buf(&mut self.buf).await? == 0 {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                return Err(ProtoError::Io(std::io::ErrorKind::UnexpectedEof.into()));
            }
        }
    }
}

/// Writes length delimited [`StreamMessage`]s, reusing one buffer.
pub(crate) struct MessageWriter {
    inner: BoxWriter,
    buf: Vec<u8>,
}

impl MessageWriter {
    pub fn new(inner: BoxWriter) -> Self {
        Self { inner, buf: Vec::new() }
    }

    pub async fn send(&mut self, msg: impl Into<StreamMessage>) -> Result<()> {
        self.buf.clear();
        msg.into().encode_length_delimited(&mut self.buf).map_err(|e| ProtoError::Protocol(format!("encode: {e}")))?;
        self.inner.write_all(&self.buf).await?;
        Ok(self.inner.flush().await?)
    }

    /// Ends the stream so the peer reads a clean end.
    pub async fn finish(&mut self) -> Result<()> {
        Ok(self.inner.shutdown().await?)
    }
}
