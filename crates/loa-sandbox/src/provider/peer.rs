//! Forwards the stub protocol to a peer's TCP port. The remote peer owns its workspace.
use super::{Channel, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::{Result, SandboxError};
use async_trait::async_trait;
use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::net::TcpStream;

pub struct PeerProvider {
    /// Configured node names or keys mapped to the port running their stub.
    pub peers: HashMap<String, SocketAddr>,
}

impl PeerProvider {
    pub fn new(peers: HashMap<String, SocketAddr>) -> Self { Self { peers } }
}

#[async_trait]
impl SandboxProvider for PeerProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Peer }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let peer = spec.options.peer.as_deref().ok_or_else(|| SandboxError::Provider { provider: "peer", message: "peer is required".into() })?;
        let address = self.peers.get(peer).ok_or_else(|| SandboxError::Provider { provider: "peer", message: format!("unknown peer: {peer}") })?;
        let stream = TcpStream::connect(address).await?;
        stream.set_nodelay(true)?;
        let (reader, writer) = stream.into_split();
        Ok(Box::new(PeerSandbox { channel: Some(Channel { reader: Box::new(reader), writer: Box::new(writer) }) }))
    }
}

struct PeerSandbox { channel: Option<Channel> }

#[async_trait]
impl Sandbox for PeerSandbox {
    fn take_channel(&mut self) -> Option<Channel> { self.channel.take() }
    fn workspace_mode(&self) -> WorkspaceMode { WorkspaceMode::Copied }
    async fn stop(mut self: Box<Self>) -> Result<()> { self.channel.take(); Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::SandboxOptions;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn forwards_bytes_without_modification() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4];
            stream.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"ping");
            stream.write_all(b"pong").await.unwrap();
        });
        let provider = PeerProvider::new(HashMap::from([("node".into(), address)]));
        let spec = SandboxSpec { name: "run".into(), workspace: "/tmp".into(), env: vec![], options: SandboxOptions { peer: Some("node".into()), ..Default::default() } };
        let mut sandbox = provider.start(&spec).await.unwrap();
        assert_eq!(sandbox.workspace_mode(), WorkspaceMode::Copied);
        let mut channel = sandbox.take_channel().unwrap();
        channel.writer.write_all(b"ping").await.unwrap();
        let mut buf = [0; 4];
        channel.reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
        sandbox.stop().await.unwrap();
        server.await.unwrap();
    }
}
