//! The peer provider over the app's node: this device runs `sandbox_task` on
//! peers that granted it [`Resource::Sandbox`], and serves its own stub to
//! peers it granted the same.

use std::path::PathBuf;
use std::sync::Arc;

use loa_trust::{Action, Resource};
use loa_node::{AddedProtocol, Endpoint, Node, NodeId, NodeSlot, PeerProtocol, Stream};
use loa_sandbox::provider::Channel;
use loa_sandbox::provider::peer::{PROTOCOL, PeerLink, serve_stub};
use loa_sandbox::{Result, SandboxError};
use tokio::sync::broadcast;

/// Version of [`PROTOCOL`] on the node.
pub const VERSION: u32 = 1;
/// What a peer's grant must cover to run a sandbox here.
pub const REQUIRED: (Resource, Action) = (Resource::Sandbox, Action::Exec);

/// Streams to the peers that granted this device sandbox access, through
/// whichever node the app runs now.
pub struct NodeLink(pub NodeSlot);

#[async_trait::async_trait]
impl PeerLink for NodeLink {
    fn running(&self) -> bool {
        self.0.get().is_some_and(|node| matches!(node.status().endpoint, Endpoint::Listening { .. }))
    }

    fn peers(&self) -> Vec<String> {
        self.0.get().map(|node| node.granting_peers(&REQUIRED).iter().map(ToString::to_string).collect()).unwrap_or_default()
    }

    fn name(&self, peer: &str) -> Option<String> {
        let node = self.0.get()?;
        let name = node.peer_name(&Node::parse_key(peer).ok()?);
        (name != peer).then_some(name)
    }

    async fn open(&self, peer: &str) -> Result<Channel> {
        let provider = |message: String| SandboxError::Provider { provider: "peer", message };
        let node = self.0.get().ok_or_else(|| provider("peer networking is not running".into()))?;
        let key: NodeId = Node::parse_key(peer).map_err(|error| provider(error.to_string()))?;
        let Stream { send, recv } = node.open_stream(key, PROTOCOL, VERSION).await.map_err(|error| provider(error.to_string()))?;
        Ok(Channel { reader: Box::new(recv), writer: Box::new(send) })
    }
}

/// A peer's sandbox run on this device that failed, for a notice.
#[derive(Clone, Debug)]
pub struct Served {
    pub peer: String,
    pub error: String,
}

/// Runs the stub for a peer whose grant the node verified.
pub struct ServeStub {
    pub stub: Option<PathBuf>,
    pub failures: broadcast::Sender<Served>,
}

#[async_trait::async_trait]
impl PeerProtocol for ServeStub {
    async fn serve(&self, peer: NodeId, stream: Stream) {
        let result = match &self.stub {
            Some(stub) => serve_stub(Channel { reader: Box::new(stream.recv), writer: Box::new(stream.send) }, stub).await,
            None => Err(SandboxError::Provider { provider: "peer", message: "loa-sandbox-stub is not installed here".into() }),
        };
        if let Err(error) = result {
            let _ = self.failures.send(Served { peer: peer.to_string(), error: error.to_string() });
        }
    }
}

/// Adds the sandbox protocol to every node the app runs.
pub fn register(slot: &NodeSlot, handler: Arc<ServeStub>) {
    slot.add_protocol(AddedProtocol { name: PROTOCOL, version: VERSION, required: REQUIRED, handler });
}
