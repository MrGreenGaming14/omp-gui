//! Authenticated iroh QUIC transport with explicit relay policy.

use crate::NodeId;
use crate::alpn::{self, Alpn};
use crate::conn::{Connection, PeerConnection, Stream, Transport};
use crate::error::{NetError, Result};
use crate::router::Router;
use async_trait::async_trait;
use bytes::Bytes;
use iroh::endpoint::{self as quic, ConnectOptions};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Default UDP listening port; use [`QuicNode::bind`] to override it.
pub const DEFAULT_PORT: u16 = 31415;

/// Only explicitly configured relays may carry peer traffic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RelayConfig {
    #[default]
    Off,
    SelfHosted(String),
}

/// One UDP endpoint serving all registered protocol versions.
pub struct QuicNode {
    endpoint: Endpoint,
    accept_task: JoinHandle<()>,
}

impl QuicNode {
    /// Binds to `bind` with the supplied persistent ed25519 identity. An empty
    /// router may dial peers but will not accept any incoming protocol.
    pub async fn bind(seed: [u8; 32], bind: SocketAddr, relay: RelayConfig, router: Router) -> Result<Self> {
        let relay_mode = match relay {
            RelayConfig::Off => RelayMode::Disabled,
            RelayConfig::SelfHosted(url) => {
                let url = url.parse::<iroh::RelayUrl>().map_err(|err| NetError::InvalidAddress(format!("relay {url}: {err}")))?;
                RelayMode::custom([url])
            }
        };
        let endpoint = quic::Builder::empty()
            .crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .clear_ip_transports()
            .bind_addr(bind)
            .map_err(|err| NetError::Bind(err.to_string()))?
            .secret_key(SecretKey::from_bytes(&seed))
            .relay_mode(relay_mode)
            .alpns(router.alpns())
            .bind()
            .await
            .map_err(|err| NetError::Bind(err.to_string()))?;
        let listening = endpoint.clone();
        let accept_task = tokio::spawn(async move {
            while let Some(incoming) = listening.accept().await {
                let router = router.clone();
                tokio::spawn(async move {
                    match incoming.await {
                        Ok(conn) => match QuicConnection::new(conn) {
                            Ok(conn) => router.dispatch(Arc::new(conn)).await,
                            Err(err) => tracing::warn!(%err, "invalid incoming ALPN"),
                        },
                        Err(err) => tracing::debug!(%err, "QUIC handshake failed"),
                    }
                });
            }
        });
        Ok(Self { endpoint, accept_task })
    }

    /// Bind on the standard UDP port on all IPv4 interfaces.
    pub async fn bind_default(seed: [u8; 32], relay: RelayConfig, router: Router) -> Result<Self> {
        Self::bind(seed, SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_PORT)), relay, router).await
    }

    /// Dial `peer` using direct socket hints. If no hint succeeds, configured
    /// endpoint address lookup and relay discovery may also locate that key.
    /// TLS selects the highest mutually supported version from the offered list.
    pub async fn dial(&self, peer: NodeId, hints: &[SocketAddr], protocol: &str, versions: &[u32]) -> Result<Connection> {
        let mut labels = alpn::offer(protocol, versions);
        if labels.is_empty() {
            return Err(NetError::NoSharedVersion { peer, protocol: protocol.to_owned() });
        }
        let addr = hints.iter().copied().fold(EndpointAddr::new(peer), |addr, hint| addr.with_ip_addr(hint));
        let opts = ConnectOptions::new().with_additional_alpns(labels.split_off(1));
        let connecting = self.endpoint.connect_with_opts(addr, &labels[0], opts).await
            .map_err(|err| NetError::Connect { peer, reason: err.to_string() })?;
        let conn = connecting.await.map_err(|err| NetError::Connect { peer, reason: err.to_string() })?;
        let actual = conn.remote_id();
        if actual != peer {
            conn.close(0x403u32.into(), b"wrong node identity");
            return Err(NetError::WrongPeer { expected: peer, actual });
        }
        let negotiated = Alpn::parse(conn.alpn())?;
        if negotiated.protocol != protocol || !versions.contains(&negotiated.version) {
            conn.close(0x404u32.into(), b"unexpected protocol version");
            return Err(NetError::NoSharedVersion { peer, protocol: protocol.to_owned() });
        }
        Ok(Arc::new(QuicConnection { conn, alpn: negotiated }))
    }

    pub fn id(&self) -> NodeId { self.endpoint.id() }

    /// Socket addresses actually bound (including the assigned port when bound to port zero).
    pub fn local_addrs(&self) -> Vec<SocketAddr> { self.endpoint.bound_sockets() }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addrs().into_iter().next().expect("bound QUIC endpoint has a socket")
    }

    /// Locally advertised direct addresses for sending as dialing hints to peers.
    pub fn direct_addrs(&self) -> Vec<SocketAddr> {
        self.endpoint.addr().ip_addrs().copied().collect()
    }

    /// Gracefully shuts down connections and stops accepting new ones.
    pub async fn close(&self) {
        self.endpoint.close().await;
        self.accept_task.abort();
    }
}

impl Drop for QuicNode {
    fn drop(&mut self) { self.accept_task.abort(); }
}

struct QuicConnection {
    conn: quic::Connection,
    alpn: Alpn,
}

impl QuicConnection {
    fn new(conn: quic::Connection) -> Result<Self> {
        let alpn = Alpn::parse(conn.alpn())?;
        Ok(Self { conn, alpn })
    }
}

#[async_trait]
impl PeerConnection for QuicConnection {
    fn remote(&self) -> NodeId { self.conn.remote_id() }
    fn alpn(&self) -> &Alpn { &self.alpn }
    fn transport(&self) -> Transport { Transport::Quic }

    async fn open_stream(&self) -> Result<Stream> {
        let (send, recv) = self.conn.open_bi().await.map_err(|err| NetError::Stream(err.to_string()))?;
        Ok(Stream { send: Box::new(send), recv: Box::new(recv) })
    }

    async fn accept_stream(&self) -> Result<Stream> {
        let (send, recv) = self.conn.accept_bi().await.map_err(|err| NetError::Stream(err.to_string()))?;
        Ok(Stream { send: Box::new(send), recv: Box::new(recv) })
    }

    fn send_datagram(&self, data: Bytes) -> Result<()> {
        if let Some(max) = self.conn.max_datagram_size()
            && data.len() > max
        {
            return Err(NetError::DatagramTooLarge { size: data.len(), max });
        }
        self.conn.send_datagram(data).map_err(|err| NetError::Datagram(err.to_string()))
    }

    async fn recv_datagram(&self) -> Result<Bytes> {
        self.conn.read_datagram().await.map_err(|err| NetError::Datagram(err.to_string()))
    }

    fn max_datagram_size(&self) -> Option<usize> { self.conn.max_datagram_size() }
    fn close(&self, code: u32, reason: &[u8]) { self.conn.close(code.into(), reason); }
    async fn closed(&self) -> String { self.conn.closed().await.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::{AllowAll, Authorizer, Decision, ProtocolHandler, CLOSE_DENIED};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::mpsc;
    use std::time::Duration;

    struct Capture(mpsc::UnboundedSender<Connection>);
    #[async_trait]
    impl ProtocolHandler for Capture {
        async fn handle(&self, conn: Connection) { let _ = self.0.send(conn); }
    }

    fn loopback() -> SocketAddr { SocketAddr::from((Ipv4Addr::LOCALHOST, 0)) }
    fn router() -> (Router, mpsc::UnboundedReceiver<Connection>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handler = Arc::new(Capture(tx));
        (Router::new(Arc::new(AllowAll)).handler("omp/session", 1, handler.clone()).handler("omp/session", 3, handler), rx)
    }

    #[tokio::test]
    async fn streams_datagrams_and_highest_shared_alpn() {
        let (router, mut accepted) = router();
        let server = QuicNode::bind([1; 32], loopback(), RelayConfig::Off, router).await.unwrap();
        let client = QuicNode::bind([2; 32], loopback(), RelayConfig::Off, Router::new(Arc::new(AllowAll))).await.unwrap();
        let dialed = client.dial(server.id(), &[server.local_addr()], "omp/session", &[1, 2, 3]).await.unwrap();
        let incoming = tokio::time::timeout(Duration::from_secs(5), accepted.recv()).await.unwrap().unwrap();
        assert_eq!(dialed.alpn().version, 3);
        assert_eq!(incoming.alpn().version, 3);
        assert_eq!(incoming.remote(), client.id());
        assert_eq!(dialed.transport(), Transport::Quic);

        let mut send = dialed.open_stream().await.unwrap();
        send.send.write_all(b"ping").await.unwrap();
        send.send.shutdown().await.unwrap();
        let mut recv = incoming.accept_stream().await.unwrap();
        let mut buf = Vec::new();
        recv.recv.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"ping");
        recv.send.write_all(b"pong").await.unwrap();
        recv.send.shutdown().await.unwrap();
        buf.clear();
        send.recv.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"pong");

        dialed.send_datagram(Bytes::from_static(b"frame")).unwrap();
        assert_eq!(tokio::time::timeout(Duration::from_secs(5), incoming.recv_datagram()).await.unwrap().unwrap(), Bytes::from_static(b"frame"));
        incoming.send_datagram(Bytes::from_static(b"input")).unwrap();
        assert_eq!(tokio::time::timeout(Duration::from_secs(5), dialed.recv_datagram()).await.unwrap().unwrap(), Bytes::from_static(b"input"));
        assert!(matches!(dialed.send_datagram(Bytes::from(vec![0; dialed.max_datagram_size().unwrap() + 1])), Err(NetError::DatagramTooLarge { .. })));
        client.close().await;
        server.close().await;
    }

    struct Deny;
    #[async_trait]
    impl Authorizer for Deny {
        async fn authorize(&self, _: &NodeId, _: &Alpn) -> Decision { Decision::Deny("not granted".into()) }
    }

    struct Never(mpsc::UnboundedSender<()>);
    #[async_trait]
    impl ProtocolHandler for Never {
        async fn handle(&self, _: Connection) {
            let _ = self.0.send(());
        }
    }

    #[tokio::test]
    async fn authorization_closes_before_handler_runs() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let router = Router::new(Arc::new(Deny)).handler("omp/control", 1, Arc::new(Never(tx)));
        let server = QuicNode::bind([3; 32], loopback(), RelayConfig::Off, router).await.unwrap();
        let client = QuicNode::bind([4; 32], loopback(), RelayConfig::Off, Router::new(Arc::new(AllowAll))).await.unwrap();
        let conn = client.dial(server.id(), &[server.local_addr()], "omp/control", &[1]).await.unwrap();
        let reason = tokio::time::timeout(Duration::from_secs(5), conn.closed()).await.unwrap();
        assert!(reason.contains(&format!("{CLOSE_DENIED}")) || reason.contains("not granted"), "{reason}");
        assert!(rx.try_recv().is_err(), "denied connection reached handler");
        client.close().await;
        server.close().await;
    }

    #[tokio::test]
    async fn unshared_protocol_is_rejected() {
        let (router, _) = router();
        let server = QuicNode::bind([5; 32], loopback(), RelayConfig::Off, router).await.unwrap();
        let client = QuicNode::bind([6; 32], loopback(), RelayConfig::Off, Router::new(Arc::new(AllowAll))).await.unwrap();
        assert!(client.dial(server.id(), &[server.local_addr()], "omp/session", &[2]).await.is_err());
        client.close().await;
        server.close().await;
    }
}
