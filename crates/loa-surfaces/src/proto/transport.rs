//! The transport port: what one `omp/surface/1` connection must offer, and
//! an in-memory implementation for tests and same process surfaces.
//!
//! A QUIC connection gives both channels: one bidirectional stream (ordered,
//! reliable) and unreliable datagrams with a size limit. `loa-net` wraps an
//! iroh connection in a [`Transport`].

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
    /// Largest datagram `send` accepts right now. `None` when the connection
    /// has no datagrams (TCP fallback). May change with the path MTU.
    fn max_size(&self) -> Option<usize>;

    /// Queues one datagram. A full send buffer drops datagrams rather than
    /// blocking; that is not an error.
    fn send(&self, datagram: Bytes) -> Result<(), DatagramError>;

    /// The next datagram. Must be cancel safe: dropping the future loses no
    /// datagram.
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

impl Transport {
    pub fn new(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
        datagrams: Arc<dyn Datagrams>,
    ) -> Self {
        Self { reader: Box::new(reader), writer: Box::new(writer), datagrams }
    }
}

/// Bytes of stream buffering in [`memory_pair`].
const MEMORY_STREAM_BUFFER: usize = 256 * 1024;
/// Datagrams queued per direction in [`memory_pair`] before new ones drop.
const MEMORY_DATAGRAM_QUEUE: usize = 1024;

/// Two connected in-memory transports. `max_datagram` of `None` behaves like
/// the TCP fallback: no datagrams.
pub fn memory_pair(max_datagram: Option<usize>) -> (Transport, Transport) {
    let (a, b) = tokio::io::duplex(MEMORY_STREAM_BUFFER);
    let (a_read, a_write) = tokio::io::split(a);
    let (b_read, b_write) = tokio::io::split(b);
    let (a_tx, b_rx) = mpsc::channel(MEMORY_DATAGRAM_QUEUE);
    let (b_tx, a_rx) = mpsc::channel(MEMORY_DATAGRAM_QUEUE);
    let a_dg = MemoryDatagrams { max: max_datagram, tx: a_tx, rx: Mutex::new(a_rx) };
    let b_dg = MemoryDatagrams { max: max_datagram, tx: b_tx, rx: Mutex::new(b_rx) };
    (Transport::new(a_read, a_write, Arc::new(a_dg)), Transport::new(b_read, b_write, Arc::new(b_dg)))
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
        if self.max.is_none() {
            return Err(DatagramError::Unsupported);
        }
        self.rx.lock().await.recv().await.ok_or(DatagramError::Closed)
    }
}

/// Largest stream message accepted: a whole frame on the TCP fallback.
pub const MAX_STREAM_MESSAGE: usize = 64 * 1024 * 1024;

/// Reads length delimited [`StreamMessage`]s. `next` is cancel safe.
pub(crate) struct MessageReader {
    inner: BoxReader,
    buf: BytesMut,
}

impl MessageReader {
    pub fn new(inner: BoxReader) -> Self {
        Self { inner, buf: BytesMut::with_capacity(8 * 1024) }
    }

    /// The next message, `None` at a clean end of stream.
    pub async fn next(&mut self) -> Result<Option<StreamMessage>> {
        loop {
            let wanted = match self.parse()? {
                Parsed::Message(msg) => return Ok(Some(msg)),
                Parsed::Need(n) => n,
            };
            self.buf.reserve(wanted.max(8 * 1024));
            if self.inner.read_buf(&mut self.buf).await? == 0 {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                return Err(ProtoError::Io(std::io::ErrorKind::UnexpectedEof.into()));
            }
        }
    }

    fn parse(&mut self) -> Result<Parsed> {
        let Some((len, prefix)) = peek_varint(&self.buf)? else {
            return Ok(Parsed::Need(1));
        };
        if len > MAX_STREAM_MESSAGE as u64 {
            return Err(ProtoError::TooLarge { len, max: MAX_STREAM_MESSAGE as u64 });
        }
        // Bounded by MAX_STREAM_MESSAGE above.
        let len = len as usize;
        if self.buf.len() < prefix + len {
            return Ok(Parsed::Need(prefix + len - self.buf.len()));
        }
        self.buf.advance(prefix);
        let body = self.buf.split_to(len).freeze();
        Ok(Parsed::Message(StreamMessage::decode(body)?))
    }
}

enum Parsed {
    Message(StreamMessage),
    Need(usize),
}

/// The varint at the start of `buf` and its byte length, `None` if `buf`
/// ends inside it.
fn peek_varint(buf: &[u8]) -> Result<Option<(u64, usize)>> {
    let mut value = 0u64;
    for (i, &byte) in buf.iter().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(Some((value, i + 1)));
        }
    }
    if buf.len() >= 10 {
        return Err(ProtoError::Protocol("message length prefix is not a varint".into()));
    }
    Ok(None)
}

/// Writes length delimited [`StreamMessage`]s, reusing one buffer.
pub(crate) struct MessageWriter {
    inner: BoxWriter,
    buf: BytesMut,
}

impl MessageWriter {
    pub fn new(inner: BoxWriter) -> Self {
        Self { inner, buf: BytesMut::new() }
    }

    pub async fn send(&mut self, msg: impl Into<StreamMessage>) -> Result<()> {
        let msg = msg.into();
        self.buf.clear();
        self.buf.reserve(msg.encoded_len() + 10);
        msg.encode_length_delimited(&mut self.buf)
            .map_err(|e| ProtoError::Protocol(format!("encode: {e}")))?;
        self.inner.write_all(&self.buf).await?;
        self.inner.flush().await?;
        Ok(())
    }

    /// Ends the stream so the peer reads a clean end.
    pub async fn finish(&mut self) -> Result<()> {
        self.inner.shutdown().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::wire::{Body, Close};

    #[tokio::test]
    async fn messages_split_across_reads_are_reassembled_in_order() {
        let (a, b) = tokio::io::duplex(3);
        let (b_read, _b_write) = tokio::io::split(b);
        let (_a_read, a_write) = tokio::io::split(a);
        let mut writer = MessageWriter::new(Box::new(a_write));
        let mut reader = MessageReader::new(Box::new(b_read));
        let sending = tokio::spawn(async move {
            for i in 0..20 {
                writer.send(Body::Close(Close { reason: format!("message {i}") })).await.unwrap();
            }
            writer.finish().await.unwrap();
        });
        for i in 0..20 {
            let msg = reader.next().await.unwrap().unwrap();
            assert_eq!(msg.body, Some(Body::Close(Close { reason: format!("message {i}") })));
        }
        assert!(reader.next().await.unwrap().is_none());
        sending.await.unwrap();
    }

    #[tokio::test]
    async fn an_oversized_length_prefix_is_refused_before_buffering() {
        let (a, b) = tokio::io::duplex(64);
        let (b_read, _b_write) = tokio::io::split(b);
        let (_a_read, mut a_write) = tokio::io::split(a);
        let mut prefix = BytesMut::new();
        prost::encoding::encode_varint(MAX_STREAM_MESSAGE as u64 + 1, &mut prefix);
        a_write.write_all(&prefix).await.unwrap();
        let mut reader = MessageReader::new(Box::new(b_read));
        assert!(matches!(reader.next().await, Err(ProtoError::TooLarge { .. })));
    }
}
