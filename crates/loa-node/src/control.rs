//! `omp/control/1` for the node: presence, grant delivery, renewal and
//! revocation; LAN browsing and tailnet probing.

use crate::{Inner, Node, NodeError, Result, grant_key};
use async_trait::async_trait;
use loa_core::now_ms;
use loa_grants::{GrantId, NodePublicKey, RenewFailure, RenewTransport};
use loa_net::NodeId;
use loa_net::alpn;
use loa_net::conn::{Connection, Stream};
use loa_net::control::{self as wire, Ack, ControlFrame, GrantRenewalResult, control_frame::Kind};
use loa_net::discovery::Discovery;
use loa_net::router::ProtocolHandler;
use loa_net::tailscale::NodeProbe;
use std::net::SocketAddr;
use std::sync::Weak;

/// The dialer's side of one control request on an open connection.
pub(crate) async fn call(node: &Node, conn: &Connection, request: Kind) -> Result<Kind> {
    let mut stream = conn.open_stream().await?;
    let hello = wire::exchange_hello(&mut stream, node.hello()).await?;
    node.observe(conn.remote(), &hello, conn.transport());
    wire::send_frame(&mut stream.send, &request.into()).await?;
    wire::recv_frame(&mut stream.recv)
        .await?
        .and_then(|frame| frame.kind)
        .ok_or_else(|| NodeError::Protocol("the peer closed without answering".into()))
}

/// Serves control streams on accepted connections.
pub(crate) struct Handler(pub(crate) Weak<Inner>);

#[async_trait]
impl ProtocolHandler for Handler {
    async fn handle(&self, conn: Connection) {
        while let Ok(stream) = conn.accept_stream().await {
            let Some(inner) = self.0.upgrade() else { return };
            let node = inner.node();
            let conn = conn.clone();
            tokio::spawn(async move {
                if let Err(error) = serve(&node, &conn, stream).await {
                    tracing::debug!(peer = %conn.remote(), %error, "control stream ended");
                }
            });
        }
    }
}

async fn serve(node: &Node, conn: &Connection, mut stream: Stream) -> Result<()> {
    let peer = conn.remote();
    let hello = wire::exchange_hello(&mut stream, node.hello()).await?;
    node.observe(peer, &hello, conn.transport());
    let Some(request) = wire::recv_frame(&mut stream.recv).await?.and_then(|f| f.kind) else {
        return Ok(());
    };
    let answer = match request {
        // The hello above already marked a saved peer online.
        Kind::Presence(_) => ack(""),
        Kind::GrantDelivery(delivery) => ack(&receive_grant(node, peer, delivery).err().map(|e| e.to_string()).unwrap_or_default()),
        Kind::GrantRenewal(renewal) => {
            let id = GrantId(String::from_utf8_lossy(&renewal.grant_id).into_owned());
            match node.grant_store().renew(node.grant_signer(), &id, &grant_key(&peer), now_ms()) {
                Ok(issued) => Kind::GrantRenewalResult(GrantRenewalResult { token: Some(issued.token), refusal: String::new() }),
                Err(error) => Kind::GrantRenewalResult(GrantRenewalResult { token: None, refusal: error.to_string() }),
            }
        }
        Kind::GrantRevocation(revocation) => {
            let id = GrantId(String::from_utf8_lossy(&revocation.grant_id).into_owned());
            ack(&revoked_by_issuer(node, peer, &id).err().map(|e| e.to_string()).unwrap_or_default())
        }
        Kind::Hello(_) | Kind::Ack(_) | Kind::GrantRenewalResult(_) => ack("not a request"),
    };
    wire::send_frame(&mut stream.send, &ControlFrame::from(answer)).await?;
    tokio::io::AsyncWriteExt::shutdown(&mut stream.send).await.map_err(loa_net::NetError::from)?;
    Ok(())
}

fn ack(error: &str) -> Kind {
    Kind::Ack(Ack { error: error.to_owned() })
}

fn receive_grant(node: &Node, sender: NodeId, delivery: wire::GrantDelivery) -> Result<()> {
    let root = if delivery.root.is_empty() {
        grant_key(&sender)
    } else {
        NodePublicKey(delivery.root.as_slice().try_into().map_err(|_| NodeError::Protocol("root key is not 32 bytes".into()))?)
    };
    let received = node.grant_store().receive(&grant_key(&node.key()), &root, &delivery.token)?;
    if received.issuer != grant_key(&sender) {
        return Err(NodeError::Refused("only a grant's issuer may deliver it".into()));
    }
    node.changed();
    Ok(())
}

fn revoked_by_issuer(node: &Node, sender: NodeId, id: &GrantId) -> Result<()> {
    let store = node.grant_store();
    let Some(held) = store.received(id)? else { return Ok(()) };
    if held.issuer != grant_key(&sender) {
        return Err(NodeError::Refused("only a grant's issuer may revoke it".into()));
    }
    let now = now_ms();
    store.record_revocation(id, &held.issuer, now)?;
    store.mark_refused(id, now)?;
    node.changed();
    Ok(())
}

/// Feeds mDNS discoveries into the node until the browse ends.
pub(crate) async fn browse(weak: Weak<Inner>, discovery: Discovery) {
    while let Some(found) = discovery.next_peer().await {
        let Some(inner) = weak.upgrade() else { break };
        inner.node().note_discovered(found);
    }
}

/// A tailnet probe: the control handshake over TCP proves the node key.
pub(crate) struct Probe(pub(crate) Node);

#[async_trait]
impl NodeProbe for Probe {
    async fn probe(&self, address: SocketAddr) -> loa_net::Result<Option<NodeId>> {
        let Ok(net) = self.0.network() else { return Ok(None) };
        let dial = tokio::time::timeout(std::time::Duration::from_secs(3), net.tcp().dial_any(address, alpn::CONTROL, &[1])).await;
        let Ok(Ok(conn)) = dial else { return Ok(None) };
        let peer = conn.remote();
        let presence = Kind::Presence(wire::Presence { online: true, last_seen_ms: Some(now_ms()) });
        let answered = tokio::time::timeout(std::time::Duration::from_secs(3), call(&self.0, &conn, presence)).await;
        conn.close(0, b"done");
        Ok(matches!(answered, Ok(Ok(_))).then_some(peer))
    }
}

/// Renews held grants by asking their issuer over control.
pub(crate) struct RenewVia(pub(crate) Node);

impl RenewTransport for RenewVia {
    fn renew(&self, issuer: NodePublicKey, grant: GrantId) -> impl Future<Output = Result<Vec<u8>, RenewFailure>> + Send {
        let node = self.0.clone();
        async move {
            let issuer = NodeId::from_bytes(&issuer.0).map_err(|e| RenewFailure::Refused(format!("issuer key: {e}")))?;
            let request = Kind::GrantRenewal(wire::GrantRenewal { grant_id: grant.as_str().as_bytes().to_vec() });
            match node.control_call(issuer, request).await {
                Ok(Kind::GrantRenewalResult(GrantRenewalResult { token: Some(token), .. })) => Ok(token),
                Ok(Kind::GrantRenewalResult(result)) => Err(RenewFailure::Refused(result.refusal)),
                Ok(other) => Err(RenewFailure::Unreachable(format!("unexpected answer {other:?}"))),
                Err(error) => Err(RenewFailure::Unreachable(error.to_string())),
            }
        }
    }
}
