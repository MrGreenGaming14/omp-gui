//! Runtime transport strategy: try QUIC first, then authenticated TCP.

use std::{net::SocketAddr, time::Duration};

use crate::{NodeId, Result, conn::Connection, endpoint::{QuicNode, RelayConfig}, router::Router, tcp::TcpNode};

/// Port used for both UDP and TCP when no explicit bind address is supplied.
pub const DEFAULT_PORT: u16 = 31415;
/// Time allowed for a direct or relayed QUIC handshake before trying TCP.
pub const QUIC_DIAL_TIMEOUT: Duration = Duration::from_millis(400);

/// An application node with both transports bound on the same configured port.
pub struct Network {
    quic: QuicNode,
    tcp: TcpNode,
}

impl Network {
    /// Starts UDP and TCP listeners with the same node identity and router.
    /// Binding port zero lets the OS select independent ephemeral ports for tests.
    pub async fn bind(seed: [u8; 32], addr: SocketAddr, relay: RelayConfig, router: Router) -> Result<Self> {
        let quic = QuicNode::bind(seed, addr, relay, router.clone()).await?;
        let tcp = TcpNode::bind(seed, addr, router).await?;
        Ok(Self { quic, tcp })
    }

    /// The local QUIC endpoint; its address can be supplied as a peer hint.
    pub fn quic(&self) -> &QuicNode { &self.quic }

    /// The local TCP listener; its address can be supplied as a peer hint.
    pub fn tcp(&self) -> &TcpNode { &self.tcp }

    /// Stops accepting connections on both transports and closes QUIC connections.
    pub async fn close(&self) {
        self.quic.close().await;
        self.tcp.close();
    }

    /// Tries QUIC first, then TCP using the supplied direct address hints. The
    /// dialer's protocol versions are offered in descending order; the server
    /// selects its highest shared version.
    pub async fn dial(
        &self,
        peer: NodeId,
        protocol: &str,
        versions: &[u32],
        quic_hints: &[SocketAddr],
        tcp_hints: &[SocketAddr],
    ) -> Result<Connection> {
        if let Ok(Ok(conn)) = tokio::time::timeout(
            QUIC_DIAL_TIMEOUT,
            self.quic.dial(peer, quic_hints, protocol, versions),
        ).await {
            return Ok(conn);
        }
        let mut last_error = None;
        for addr in tcp_hints {
            match self.tcp.dial(peer, *addr, protocol, versions).await {
                Ok(conn) => return Ok(conn),
                Err(err) => last_error = Some(err),
            }
        }
        Err(last_error.unwrap_or(crate::NetError::Connect {
            peer,
            reason: "QUIC failed and no TCP address hints were available".into(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{alpn::SESSION, conn::{Connection, Transport}, router::{AllowAll, ProtocolHandler}};
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Reply;

    #[async_trait]
    impl ProtocolHandler for Reply {
        async fn handle(&self, conn: Connection) {
            if let Ok(mut stream) = conn.accept_stream().await {
                let mut request = [0; 4];
                if stream.recv.read_exact(&mut request).await.is_ok() && &request == b"ping" {
                    let _ = stream.send.write_all(b"pong").await;
                    let _ = stream.send.shutdown().await;
                }
            }
        }
    }

    #[tokio::test]
    async fn falls_back_to_authenticated_tcp_when_udp_does_not_answer() {
        let router = Router::new(Arc::new(AllowAll)).handler(SESSION, 1, Arc::new(Reply));
        let bind = SocketAddr::from(([127, 0, 0, 1], 0));
        let server = Network::bind([31; 32], bind, RelayConfig::Off, router.clone()).await.unwrap();
        let client = Network::bind([32; 32], bind, RelayConfig::Off, router).await.unwrap();
        // A reserved UDP socket responds to no iroh handshake. No timing sleeps
        // or dependence on another machine's firewall are needed.
        let blocked_udp = tokio::net::UdpSocket::bind(bind).await.unwrap();
        let conn = client.dial(
            server.tcp().id(),
            SESSION,
            &[1],
            &[blocked_udp.local_addr().unwrap()],
            &[server.tcp().local_addr()],
        ).await.unwrap();
        assert_eq!(conn.transport(), Transport::Tcp);
        let mut stream = conn.open_stream().await.unwrap();
        stream.send.write_all(b"ping").await.unwrap();
        stream.send.shutdown().await.unwrap();
        let mut reply = Vec::new();
        stream.recv.read_to_end(&mut reply).await.unwrap();
        assert_eq!(&reply, b"pong");
    }
}
