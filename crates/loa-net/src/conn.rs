//! The transport independent connection every protocol handler works with.

use crate::NodeId;
use crate::alpn::Alpn;
use crate::error::Result;
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// Which transport carries a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Transport {
    /// iroh QUIC over UDP on a direct path.
    Quic,
    /// iroh QUIC carried by a relay server, when no direct path works.
    Relay,
    /// TLS 1.3 over TCP with multiplexed streams, for networks that block UDP.
    Tcp,
}

/// Write half of an ordered, reliable stream.
pub type SendHalf = Box<dyn AsyncWrite + Send + Unpin>;
/// Read half of an ordered, reliable stream.
pub type RecvHalf = Box<dyn AsyncRead + Send + Unpin>;

/// One bidirectional stream. Shutting down `send` (`AsyncWriteExt::shutdown`)
/// ends the peer's `recv` with EOF.
pub struct Stream {
    pub send: SendHalf,
    pub recv: RecvHalf,
}

/// An authenticated connection to one peer on one negotiated protocol version.
///
/// Implementations: QUIC through iroh, and TLS over TCP. Streams are ordered
/// and reliable. Datagrams are unreliable and unordered and at most
/// [`PeerConnection::max_datagram_size`] bytes; on TCP they are delivered in
/// order but dropped when the receiver falls behind.
#[async_trait]
pub trait PeerConnection: Send + Sync {
    /// The peer's node key, proven during the handshake.
    fn remote(&self) -> NodeId;
    /// The negotiated protocol and version.
    fn alpn(&self) -> &Alpn;
    fn transport(&self) -> Transport;
    /// Opens a new stream. The peer sees it from `accept_stream` once data
    /// is written on it.
    async fn open_stream(&self) -> Result<Stream>;
    /// Waits for the peer to open a stream.
    async fn accept_stream(&self) -> Result<Stream>;
    /// Sends one datagram without waiting.
    fn send_datagram(&self, data: Bytes) -> Result<()>;
    /// Waits for the next datagram.
    async fn recv_datagram(&self) -> Result<Bytes>;
    /// Largest datagram [`PeerConnection::send_datagram`] accepts, `None` when
    /// the peer doesn't take datagrams.
    fn max_datagram_size(&self) -> Option<usize>;
    /// Closes the connection with an application code and reason.
    fn close(&self, code: u32, reason: &[u8]);
    /// Resolves when the connection is closed by either side, with the reason.
    async fn closed(&self) -> String;
}

/// A shared connection handle.
pub type Connection = Arc<dyn PeerConnection>;
