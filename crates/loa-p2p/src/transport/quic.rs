//! The iroh QUIC adapter, with an explicit relay policy.

use super::{Connection, PeerConnection, Router, Stream, Transport, TransportKind, check_alpn};
use crate::{NodeError, NodeId, Result, net};
use async_trait::async_trait;
use iroh::endpoint::{self as quic};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Only explicitly configured relays may carry peer traffic.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RelayConfig {
    #[default]
    Off,
    SelfHosted(String),
}

/// One UDP endpoint serving every label of its router.
pub struct Quic {
    endpoint: Endpoint,
    accept: JoinHandle<()>,
}

impl Quic {
    pub async fn bind(seed: [u8; 32], bind: SocketAddr, relay: RelayConfig, router: Router) -> Result<Self> {
        let relay = match relay {
            RelayConfig::Off => RelayMode::Disabled,
            RelayConfig::SelfHosted(url) => RelayMode::custom([url.parse::<iroh::RelayUrl>().map_err(|e| NodeError::InvalidAddress(url, e.to_string()))?]),
        };
        let endpoint = quic::Builder::empty()
            .crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .clear_ip_transports()
            .bind_addr(bind)
            .map_err(net)?
            .secret_key(SecretKey::from_bytes(&seed))
            .relay_mode(relay)
            .alpns(router.alpns())
            .bind()
            .await
            .map_err(net)?;
        let listening = endpoint.clone();
        let accept = tokio::spawn(async move {
            while let Some(incoming) = listening.accept().await {
                let router = router.clone();
                tokio::spawn(async move {
                    match incoming.await {
                        Ok(conn) => router.dispatch(Arc::new(QuicConnection(conn))).await,
                        Err(err) => tracing::debug!(%err, "QUIC handshake failed"),
                    }
                });
            }
        });
        Ok(Self { endpoint, accept })
    }
}

impl Drop for Quic {
    fn drop(&mut self) { self.accept.abort(); }
}

#[async_trait]
impl Transport for Quic {
    async fn dial(&self, peer: &NodeId, hints: &[SocketAddr], alpn: &[u8]) -> Result<Connection> {
        let peer = *peer;
        let addr = hints.iter().fold(EndpointAddr::new(peer), |addr, hint| addr.with_ip_addr(*hint));
        let conn = self.endpoint.connect(addr, alpn).await.map_err(|e| NodeError::Connect(peer, e.to_string()))?;
        if conn.remote_id() != peer {
            conn.close(0x403u32.into(), b"wrong node identity");
            return Err(NodeError::WrongPeer(peer, conn.remote_id()));
        }
        check_alpn(peer, alpn, Some(conn.alpn()))?;
        Ok(Arc::new(QuicConnection(conn)))
    }

    async fn dial_any(&self, addr: SocketAddr, _: &[u8]) -> Result<Connection> {
        Err(NodeError::InvalidAddress(addr.to_string(), "QUIC needs the node's key".into()))
    }

    fn local_addrs(&self) -> Vec<SocketAddr> { self.endpoint.bound_sockets() }

    async fn close(&self) {
        self.endpoint.close().await;
        self.accept.abort();
    }
}

struct QuicConnection(quic::Connection);

#[async_trait]
impl PeerConnection for QuicConnection {
    fn remote(&self) -> NodeId { self.0.remote_id() }
    fn alpn(&self) -> &[u8] { self.0.alpn() }
    fn kind(&self) -> TransportKind {
        if self.0.paths().iter().any(|path| path.is_selected() && path.is_relay()) { TransportKind::Relay } else { TransportKind::Quic }
    }
    async fn open_stream(&self) -> Result<Stream> {
        let (send, recv) = self.0.open_bi().await.map_err(net)?;
        Ok(Stream { send: Box::new(send), recv: Box::new(recv) })
    }
    async fn accept_stream(&self) -> Result<Stream> {
        let (send, recv) = self.0.accept_bi().await.map_err(net)?;
        Ok(Stream { send: Box::new(send), recv: Box::new(recv) })
    }
    fn close(&self, code: u32, reason: &[u8]) { self.0.close(code.into(), reason); }
    async fn closed(&self) -> String { self.0.closed().await.to_string() }
}
