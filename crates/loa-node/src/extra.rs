//! Protocols the app adds to the node (e.g. `loa-sandbox/1`): each stream
//! starts with the grant handshake of `omp/session/1`, and only a requester
//! whose proven grants cover the protocol's required resource and action
//! reaches the app's handler with the raw stream.

use loa_util::RwLockExt;
use crate::share::{challenge_grants, expect, present_grants, send};
use crate::{Inner, Node, NodeError, Result, now_ms};
use async_trait::async_trait;
use loa_grants::{Action, Request, Resource, Revocations, VerifiedGrant};
use loa_net::NodeId;
use loa_net::conn::{Connection, Stream};
use loa_net::router::ProtocolHandler;
use loa_share::{Ack, frame::Body};
use std::pin::Pin;
use std::sync::{Arc, RwLock, Weak};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The app's side of an added protocol: one call per accepted stream, after
/// the requester proved a grant covering the protocol's requirement.
#[async_trait]
pub trait PeerProtocol: Send + Sync {
    async fn serve(&self, peer: NodeId, stream: Stream);
}

/// A protocol added with [`Node::add_protocol`].
#[derive(Clone)]
pub struct AddedProtocol {
    pub name: &'static str,
    pub version: u32,
    /// The grant a requester must prove: an action on a resource.
    pub required: (Resource, Action),
    pub handler: Arc<dyn PeerProtocol>,
}

/// The node the app runs now, plus the protocols areas added. The app
/// replaces the node when it opens it again; every holder of a clone sees
/// the new one, and every node set here serves the added protocols.
#[derive(Clone, Default)]
pub struct NodeSlot(Arc<RwLock<Slot>>);

#[derive(Default)]
struct Slot {
    node: Option<Node>,
    added: Vec<AddedProtocol>,
}

impl NodeSlot {
    pub fn get(&self) -> Option<Node> {
        self.0.read_ok().node.clone()
    }

    /// Puts `node` in the slot; it serves every protocol added so far from
    /// its next start.
    pub fn set(&self, node: Option<Node>) {
        let mut slot = self.0.write_ok();
        if let Some(node) = &node {
            for added in &slot.added {
                node.add_protocol(added.name, added.version, added.required.clone(), Arc::clone(&added.handler));
            }
        }
        slot.node = node;
    }

    /// Adds a protocol to the current node and every later one (see
    /// [`Node::add_protocol`]); a node serves it from its next start.
    pub fn add_protocol(&self, protocol: AddedProtocol) {
        let mut slot = self.0.write_ok();
        if let Some(node) = &slot.node {
            node.add_protocol(protocol.name, protocol.version, protocol.required.clone(), Arc::clone(&protocol.handler));
        }
        slot.added.retain(|added| (added.name, added.version) != (protocol.name, protocol.version));
        slot.added.push(protocol);
    }
}

/// Whether `grant` covers `action` on `resource` now and none of its chain
/// was revoked here.
pub(crate) fn covers(node: &Node, grant: &VerifiedGrant, (resource, action): &(Resource, Action), now: i64) -> bool {
    let request = match resource {
        Resource::Sandbox => Request::sandbox(*action),
        Resource::Ssh { host } => Request::ssh(*action, host),
        Resource::Surface { uri } => Request::surface(*action, uri),
        // Session resources go through `omp/session/1`.
        _ => return false,
    };
    grant.expires_ms() > now
        && !grant.chain().iter().any(|link| node.grant_store().is_revoked(&link.grant))
        && grant.authorize(&request, now).is_ok()
}

/// Serves an added protocol's streams.
pub(crate) struct Handler {
    pub(crate) inner: Weak<Inner>,
    pub(crate) protocol: AddedProtocol,
}

#[async_trait]
impl ProtocolHandler for Handler {
    async fn handle(&self, conn: Connection) {
        while let Ok(stream) = conn.accept_stream().await {
            let Some(inner) = self.inner.upgrade() else { return };
            let node = inner.node();
            let peer = conn.remote();
            let protocol = self.protocol.clone();
            tokio::spawn(async move {
                if let Err(error) = serve(&node, peer, stream, protocol).await {
                    tracing::debug!(%peer, %error, "added protocol stream refused");
                }
            });
        }
    }
}

async fn serve(node: &Node, peer: NodeId, mut stream: Stream, protocol: AddedProtocol) -> Result<()> {
    // The requester's opening frame makes the stream visible here.
    expect(&mut stream.recv).await?;
    let grants = challenge_grants(node, peer, &mut stream).await?;
    let now = now_ms();
    if !grants.iter().any(|grant| covers(node, grant, &protocol.required, now)) {
        let error = format!("no grant for {} on this node", protocol.name);
        send(&mut stream.send, Body::Ack(Ack { error: error.clone() })).await?;
        return Err(NodeError::Refused(error));
    }
    send(&mut stream.send, Body::Ack(Ack { error: String::new() })).await?;
    protocol.handler.serve(peer, stream).await;
    Ok(())
}

/// The requester side: a stream on `name`/`version` to `peer`, past the
/// grant handshake. Its halves keep the connection open.
pub(crate) async fn open(node: &Node, peer: NodeId, name: &'static str, version: u32) -> Result<Stream> {
    let conn = node.dial_versions(peer, name, &[version]).await?;
    let mut stream = conn.open_stream().await?;
    send(&mut stream.send, Body::Ack(Ack { error: String::new() })).await?;
    present_grants(node, peer, &mut stream).await?;
    match expect(&mut stream.recv).await? {
        Body::Ack(Ack { error }) if error.is_empty() => Ok(Stream {
            send: Box::new(Held { half: stream.send, _conn: Arc::clone(&conn) }),
            recv: Box::new(Held { half: stream.recv, _conn: conn }),
        }),
        Body::Ack(Ack { error }) => Err(NodeError::Refused(error)),
        _ => Err(NodeError::Protocol("expected the owner's answer".into())),
    }
}

/// A stream half that holds its connection open while it lives.
struct Held<H> {
    half: H,
    _conn: Connection,
}

impl<H: AsyncRead + Unpin> AsyncRead for Held<H> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.half).poll_read(cx, buf)
    }
}

impl<H: AsyncWrite + Unpin> AsyncWrite for Held<H> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.half).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.half).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.half).poll_shutdown(cx)
    }
}
