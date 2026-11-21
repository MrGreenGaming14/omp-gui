//! `omp/control/1`: both sides send a hello, then the dialer sends one
//! request (presence, grant delivery, renewal or revocation) and the listener
//! answers it. Any node may open it; a connection never makes it a peer.

use crate::discovery::NodeProbe;
use crate::node::Node;
use crate::transport::{Connection, Stream};
use crate::wire::{self, Control, ControlFrame, GrantRenewal, GrantRenewalResult, Presence};
use crate::{NodeError, NodeId, Result};
use async_trait::async_trait;
use loa_trust::{GrantId, PublicKey, RenewFailure, RenewTransport};
use loa_util::time::now_ms;
use std::net::SocketAddr;
use std::time::Duration;

async fn hello(node: &Node, conn: &Connection, stream: &mut Stream) -> Result<()> {
    let Some(Control::Hello(hello)) = wire::call::<_, ControlFrame>(stream, &ControlFrame::from(Control::Hello(node.hello()))).await?.body else {
        return Err(NodeError::Protocol("first control frame must be hello".into()));
    };
    if hello.app_version.is_empty() || hello.protocols.iter().any(|p| p.name.is_empty() || p.version == 0) {
        return Err(NodeError::Protocol("invalid peer hello".into()));
    }
    node.observe(conn.remote(), &hello, conn.kind());
    Ok(())
}

/// The dialer's side of one request on an open connection.
pub(crate) async fn call(node: &Node, conn: &Connection, request: Control) -> Result<Control> {
    let mut stream = conn.open_stream().await?;
    hello(node, conn, &mut stream).await?;
    let answer: ControlFrame = wire::call(&mut stream, &ControlFrame::from(request)).await?;
    answer.body.ok_or_else(|| NodeError::Protocol("empty answer".into()))
}

pub(crate) fn presence() -> Control { Control::Presence(Presence { online: true, last_seen_ms: Some(now_ms()) }) }

pub(crate) async fn serve(node: Node, conn: Connection, mut stream: Stream) -> Result<()> {
    let peer = conn.remote();
    hello(&node, &conn, &mut stream).await?;
    let Some(ControlFrame { body: Some(request) }) = wire::recv(&mut stream.recv).await? else { return Ok(()) };
    let grants = &node.0.grants;
    let id = |bytes: Vec<u8>| GrantId(String::from_utf8_lossy(&bytes).into_owned());
    let result = match request {
        // The hello already marked a saved peer online.
        Control::Presence(_) => Ok(()),
        Control::GrantDelivery(delivery) => grants.receive(peer, &delivery.token, &delivery.root),
        Control::GrantRevocation(revocation) => grants.revoked_by(peer, &id(revocation.grant_id)),
        Control::GrantRenewal(renewal) => {
            let (token, refusal) = match grants.store.renew(&grants.identity, &id(renewal.grant_id), &PublicKey::from(peer), now_ms()) {
                Ok(issued) => (Some(issued.token), String::new()),
                Err(error) => (None, error.to_string()),
            };
            return wire::send(&mut stream.send, &ControlFrame::from(Control::GrantRenewalResult(GrantRenewalResult { token, refusal }))).await;
        }
        _ => Err(NodeError::Protocol("not a request".into())),
    };
    if result.is_ok() {
        node.changed();
    }
    wire::ack::<ControlFrame>(&mut stream.send, result.err().map(|e| e.to_string())).await?;
    Ok(tokio::io::AsyncWriteExt::shutdown(&mut stream.send).await?)
}

/// A tailnet probe: the control handshake over TCP proves the node key.
pub(crate) struct Probe(pub Node);

#[async_trait]
impl NodeProbe for Probe {
    async fn probe(&self, address: SocketAddr) -> Option<NodeId> {
        let net = self.0.network().ok()?;
        let wait = Duration::from_secs(3);
        let conn = tokio::time::timeout(wait, net.dial_any(address, wire::CONTROL)).await.ok()?.ok()?;
        let answered = tokio::time::timeout(wait, call(&self.0, &conn, presence())).await;
        conn.close(0, b"done");
        matches!(answered, Ok(Ok(_))).then(|| conn.remote())
    }
}

/// Renews held grants by asking their issuer over control.
pub(crate) struct RenewVia(pub Node);

impl RenewTransport for RenewVia {
    fn renew(&self, issuer: PublicKey, grant: GrantId) -> impl Future<Output = Result<Vec<u8>, RenewFailure>> + Send {
        let node = self.0.clone();
        async move {
            let issuer = NodeId::try_from(issuer).map_err(|e| RenewFailure::Refused(format!("issuer key: {e}")))?;
            let request = Control::GrantRenewal(GrantRenewal { grant_id: grant.as_str().as_bytes().to_vec() });
            match node.control_call(issuer, request).await {
                Ok(Control::GrantRenewalResult(GrantRenewalResult { token: Some(token), .. })) => Ok(token),
                Ok(Control::GrantRenewalResult(result)) => Err(RenewFailure::Refused(result.refusal)),
                Ok(other) => Err(RenewFailure::Unreachable(format!("unexpected answer {other:?}"))),
                Err(error) => Err(RenewFailure::Unreachable(error.to_string())),
            }
        }
    }
}
