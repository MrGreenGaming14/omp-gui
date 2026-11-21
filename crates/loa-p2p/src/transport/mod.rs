//! Authenticated connections: the [`Transport`] port, its iroh QUIC and
//! TLS+yamux TCP adapters, the ALPN [`Router`] and the [`FallbackDialer`].

mod quic;
mod tcp;

pub use quic::{Quic, RelayConfig};
pub use tcp::Tcp;

use crate::{NodeError, NodeId, Result};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

/// What carries a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TransportKind {
    Quic,
    /// QUIC through a relay server.
    Relay,
    Tcp,
}

pub type SendHalf = Box<dyn AsyncWrite + Send + Unpin>;
pub type RecvHalf = Box<dyn AsyncRead + Send + Unpin>;

/// One ordered, reliable bidirectional stream; shutting `send` ends the peer's `recv`.
pub struct Stream {
    pub send: SendHalf,
    pub recv: RecvHalf,
}

/// A connection to one peer on one protocol label.
#[async_trait]
pub trait PeerConnection: Send + Sync {
    /// The peer's key, proven during the handshake.
    fn remote(&self) -> NodeId;
    fn alpn(&self) -> &[u8];
    fn kind(&self) -> TransportKind;
    async fn open_stream(&self) -> Result<Stream>;
    async fn accept_stream(&self) -> Result<Stream>;
    fn close(&self, code: u32, reason: &[u8]);
    /// Resolves with the reason once either side closed.
    async fn closed(&self) -> String;
}

pub type Connection = Arc<dyn PeerConnection>;

/// Port: one way to reach peers.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Dials `peer` on its address hints; the handshake must prove its key.
    async fn dial(&self, peer: &NodeId, hints: &[SocketAddr], alpn: &[u8]) -> Result<Connection>;
    /// Dials whatever node answers at `addr`, for probing addresses learned without a key.
    async fn dial_any(&self, addr: SocketAddr, alpn: &[u8]) -> Result<Connection>;
    fn local_addrs(&self) -> Vec<SocketAddr>;
    /// Stops accepting connections.
    async fn close(&self);
}

/// Serves one protocol on accepted connections, for the connection's life.
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    async fn handle(&self, conn: Connection);
}

/// Close code when no handler serves the negotiated label.
pub const CLOSE_NO_HANDLER: u32 = 0x404;

/// Handlers keyed by exact protocol label.
#[derive(Clone, Default)]
pub struct Router(BTreeMap<Vec<u8>, Arc<dyn ProtocolHandler>>);

impl Router {
    pub fn handler(mut self, alpn: impl Into<Vec<u8>>, handler: Arc<dyn ProtocolHandler>) -> Self {
        self.0.insert(alpn.into(), handler);
        self
    }

    pub fn alpns(&self) -> Vec<Vec<u8>> { self.0.keys().cloned().collect() }

    pub async fn dispatch(&self, conn: Connection) {
        match self.0.get(conn.alpn()) {
            Some(handler) => handler.handle(conn).await,
            None => conn.close(CLOSE_NO_HANDLER, b"no handler"),
        }
    }
}

/// Fails unless the negotiated label is the one asked for.
pub(crate) fn check_alpn(peer: NodeId, asked: &[u8], got: Option<&[u8]>) -> Result<()> {
    match got {
        Some(got) if got == asked => Ok(()),
        _ => Err(NodeError::Connect(peer, format!("the peer doesn't serve {}", String::from_utf8_lossy(asked)))),
    }
}

/// Tries each transport in order; the first gets `first_timeout`.
pub struct FallbackDialer {
    pub transports: Vec<Arc<dyn Transport>>,
    pub first_timeout: Duration,
}

impl FallbackDialer {
    async fn each<'a, F>(&'a self, peer: Option<NodeId>, attempt: impl Fn(&'a dyn Transport) -> F) -> Result<Connection>
    where F: Future<Output = Result<Connection>> + 'a {
        let mut last = None;
        for (i, transport) in self.transports.iter().enumerate() {
            let result = match i {
                0 => tokio::time::timeout(self.first_timeout, attempt(&**transport)).await.unwrap_or(Err(NodeError::Timeout("the first transport"))),
                _ => attempt(&**transport).await,
            };
            match result {
                Ok(conn) => return Ok(conn),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| NodeError::Unavailable(format!("no transport reaches {peer:?}"))))
    }

    pub async fn dial(&self, peer: NodeId, hints: &[SocketAddr], alpn: &[u8]) -> Result<Connection> {
        self.each(Some(peer), |t| t.dial(&peer, hints, alpn)).await
    }

    pub async fn dial_any(&self, addr: SocketAddr, alpn: &[u8]) -> Result<Connection> { self.each(None, |t| t.dial_any(addr, alpn)).await }

    pub async fn close(&self) {
        for transport in &self.transports {
            transport.close().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc;

    struct Capture(mpsc::UnboundedSender<Connection>);
    #[async_trait]
    impl ProtocolHandler for Capture {
        async fn handle(&self, conn: Connection) { let _ = self.0.send(conn); }
    }

    const ALPN: &[u8] = b"omp/test/1";
    fn loopback() -> SocketAddr { SocketAddr::from(([127, 0, 0, 1], 0)) }
    fn router() -> (Router, mpsc::UnboundedReceiver<Connection>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Router::default().handler(ALPN, Arc::new(Capture(tx))), rx)
    }
    fn key(seed: u8) -> NodeId { iroh::SecretKey::from_bytes(&[seed; 32]).public() }

    /// Streams both ways, then refusal of a wrong key and of an unserved label.
    async fn exercise(server: Arc<dyn Transport>, client: Arc<dyn Transport>, server_key: NodeId, mut accepted: mpsc::UnboundedReceiver<Connection>) {
        let hints = server.local_addrs();
        let dialed = client.dial(&server_key, &hints, ALPN).await.unwrap();
        let incoming = tokio::time::timeout(Duration::from_secs(5), accepted.recv()).await.unwrap().unwrap();
        assert_eq!(incoming.remote(), key(2));
        let mut out = dialed.open_stream().await.unwrap();
        out.send.write_all(b"ping").await.unwrap();
        out.send.shutdown().await.unwrap();
        let mut inc = incoming.accept_stream().await.unwrap();
        let mut buf = Vec::new();
        inc.recv.read_to_end(&mut buf).await.unwrap();
        inc.send.write_all(b"pong").await.unwrap();
        inc.send.shutdown().await.unwrap();
        out.recv.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"pingpong");
        assert!(client.dial(&key(9), &hints, ALPN).await.is_err(), "a different key is refused");
        assert!(client.dial(&server_key, &hints, b"omp/test/2").await.is_err(), "labels match exactly");
        client.close().await;
        server.close().await;
    }

    #[tokio::test]
    async fn quic_loopback() {
        let (router, rx) = router();
        let server = Arc::new(Quic::bind([1; 32], loopback(), RelayConfig::Off, router).await.unwrap());
        let client = Arc::new(Quic::bind([2; 32], loopback(), RelayConfig::Off, Router::default()).await.unwrap());
        exercise(server, client, key(1), rx).await;
    }

    #[tokio::test]
    async fn tcp_loopback() {
        let (router, rx) = router();
        let server = Arc::new(Tcp::bind([1; 32], loopback(), router).await.unwrap());
        let client = Arc::new(Tcp::bind([2; 32], loopback(), Router::default()).await.unwrap());
        exercise(server, client, key(1), rx).await;
    }

    #[tokio::test]
    async fn falls_back_to_tcp_when_udp_does_not_answer() {
        let (router, mut rx) = router();
        let tcp = Tcp::bind([1; 32], loopback(), router).await.unwrap();
        let quic = Quic::bind([2; 32], loopback(), RelayConfig::Off, Router::default()).await.unwrap();
        let client = Tcp::bind([2; 32], loopback(), Router::default()).await.unwrap();
        let blocked = tokio::net::UdpSocket::bind(loopback()).await.unwrap();
        let dialer = FallbackDialer { transports: vec![Arc::new(quic), Arc::new(client)], first_timeout: Duration::from_millis(400) };
        let hints = [blocked.local_addr().unwrap(), tcp.local_addrs()[0]];
        assert_eq!(dialer.dial(key(1), &hints, ALPN).await.unwrap().kind(), TransportKind::Tcp);
        assert!(rx.recv().await.is_some());
    }
}
