//! Protocols the app adds to the node (e.g. `loa-sandbox/1`): each stream
//! passes the grant handshake, and only a requester whose grants cover the
//! required resource and action reaches the app's handler with the raw stream.

use crate::node::Node;
use crate::transport::{Connection, Stream};
use crate::wire::{self, Ack, Body, Frame};
use crate::{NodeError, NodeId, Result};
use async_trait::async_trait;
use loa_trust::{Action, Resource};
use loa_util::RwLockExt;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The app's side of an added protocol: one call per stream a covering grant let through.
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

impl AddedProtocol {
    /// The exact protocol label, `<name>/<version>`.
    pub fn label(&self) -> Vec<u8> { label(self.name, self.version) }
}

pub(crate) fn label(name: &str, version: u32) -> Vec<u8> { format!("{name}/{version}").into_bytes() }

/// The node the app runs now plus the protocols areas added; every node set
/// here serves them from its next start.
#[derive(Clone, Default)]
pub struct NodeSlot(Arc<RwLock<(Option<Node>, Vec<AddedProtocol>)>>);

impl NodeSlot {
    pub fn get(&self) -> Option<Node> { self.0.read_ok().0.clone() }

    pub fn set(&self, node: Option<Node>) {
        let mut slot = self.0.write_ok();
        if let Some(node) = &node {
            slot.1.iter().for_each(|added| node.add(added.clone()));
        }
        slot.0 = node;
    }

    /// Adds a protocol to the current node and every later one.
    pub fn add_protocol(&self, protocol: AddedProtocol) {
        let mut slot = self.0.write_ok();
        if let Some(node) = &slot.0 {
            node.add(protocol.clone());
        }
        slot.1.retain(|added| added.label() != protocol.label());
        slot.1.push(protocol);
    }
}

pub(crate) async fn serve(node: Node, conn: Connection, mut stream: Stream, protocol: AddedProtocol) -> Result<()> {
    let peer = conn.remote();
    // The requester's opening frame makes the stream visible here.
    wire::expect::<Frame>(&mut stream.recv).await?;
    node.0.grants.challenge(peer, &mut stream).await?;
    let (resource, action) = &protocol.required;
    let error = (!node.0.grants.covers(peer, resource, *action)).then(|| format!("no grant for {} on this node", protocol.name));
    wire::ack::<Frame>(&mut stream.send, error.clone()).await?;
    match error {
        Some(error) => Err(NodeError::Refused(error)),
        None => Ok(protocol.handler.serve(peer, stream).await),
    }
}

/// The requester side: a stream past the grant handshake; its halves keep the connection open.
pub(crate) async fn open(node: &Node, peer: NodeId, name: &str, version: u32) -> Result<Stream> {
    let conn = node.dial(peer, &label(name, version)).await?;
    let mut stream = conn.open_stream().await?;
    wire::send(&mut stream.send, &Frame::from(Body::Ack(Ack::default()))).await?;
    node.0.grants.present(peer, &mut stream).await?;
    wire::expect::<Frame>(&mut stream.recv).await?;
    Ok(Stream { send: Box::new(Held(stream.send, conn.clone())), recv: Box::new(Held(stream.recv, conn)) })
}

/// A stream half that holds its connection open while it lives.
struct Held<H>(H, #[allow(dead_code)] Connection);

impl<H: AsyncRead + Unpin> AsyncRead for Held<H> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> { Pin::new(&mut self.0).poll_read(cx, buf) }
}

impl<H: AsyncWrite + Unpin> AsyncWrite for Held<H> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> { Pin::new(&mut self.0).poll_write(cx, buf) }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> { Pin::new(&mut self.0).poll_flush(cx) }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> { Pin::new(&mut self.0).poll_shutdown(cx) }
}
