//! Runs the stub on a peer node. The host opens an authenticated stream to
//! the peer through a [`PeerLink`] (the app backs it with `loa-net` and the
//! peer's grant); the peer answers it with [`serve_stub`], which runs the stub
//! in a fresh, empty directory. The workspace is copied in and out over the
//! same stream (see [`crate::sync`]).
use super::{Channel, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::{Result, SandboxError};
use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;

/// Protocol name for peer sandbox streams on the node's router.
pub const PROTOCOL: &str = "loa-sandbox";

/// Authenticated streams to peers that granted this node sandbox access.
#[async_trait]
pub trait PeerLink: Send + Sync {
    /// Whether peer networking runs now; peers can't be reached otherwise.
    fn running(&self) -> bool {
        true
    }
    /// Names or keys of the peers that can run a sandbox now.
    fn peers(&self) -> Vec<String>;
    /// What the user calls `peer`, when it has a name besides its key.
    fn name(&self, _peer: &str) -> Option<String> {
        None
    }
    /// Opens a stream to the peer's [`serve_stub`].
    async fn open(&self, peer: &str) -> Result<Channel>;
}

pub struct PeerProvider {
    pub link: Arc<dyn PeerLink>,
}

impl PeerProvider {
    pub fn new(link: Arc<dyn PeerLink>) -> Self { Self { link } }
}

#[async_trait]
impl SandboxProvider for PeerProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Peer }
    fn describe(&self) -> String {
        let peers: Vec<String> = self
            .link
            .peers()
            .into_iter()
            .map(|peer| match self.link.name(&peer) {
                Some(name) => format!("{peer} ({name})"),
                None => peer,
            })
            .collect();
        match peers.as_slice() {
            [] => "a peer node that granted sandbox access (none right now); set options.peer".into(),
            peers => format!("a peer node; set options.peer to one of {}", peers.join(", ")),
        }
    }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let peer = spec.options.peer.as_deref().ok_or_else(|| SandboxError::Provider { provider: "peer", message: "peer is required".into() })?;
        if !self.link.peers().iter().any(|known| known == peer) {
            return Err(SandboxError::Provider { provider: "peer", message: format!("no sandbox grant from peer {peer}") });
        }
        let channel = self.link.open(peer).await?;
        Ok(Box::new(PeerSandbox { channel: Some(channel) }))
    }
}

struct PeerSandbox { channel: Option<Channel> }

#[async_trait]
impl Sandbox for PeerSandbox {
    fn take_channel(&mut self) -> Option<Channel> { self.channel.take() }
    fn workspace_mode(&self) -> WorkspaceMode { WorkspaceMode::Copied }
    async fn stop(mut self: Box<Self>) -> Result<()> { self.channel.take(); Ok(()) }
}

/// The peer side of a sandbox stream: runs `stub` with an empty environment
/// in a new temporary directory and pipes the stream to it until either side
/// ends. The directory is removed afterwards.
pub async fn serve_stub(channel: Channel, stub: &Path) -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut child = Command::new(stub)
        .arg(root.path())
        .current_dir(root.path())
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let mut stdout = child.stdout.take().expect("piped stdout");
    let Channel { mut reader, mut writer } = channel;
    let inbound = async {
        let copied = tokio::io::copy(&mut reader, &mut stdin).await;
        // Closing the stub's stdin ends it, which ends the outbound copy.
        drop(stdin);
        copied
    };
    let outbound = async {
        let copied = tokio::io::copy(&mut stdout, &mut writer).await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut writer).await;
        copied
    };
    let (inbound, outbound) = tokio::join!(inbound, outbound);
    if child.try_wait()?.is_none() {
        child.kill().await?;
    }
    child.wait().await?;
    inbound?;
    outbound?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::SandboxOptions;
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Hands out one end of an in-memory stream per open and keeps the other.
    struct Duplex { far: Mutex<Vec<tokio::io::DuplexStream>> }

    #[async_trait]
    impl PeerLink for Duplex {
        fn peers(&self) -> Vec<String> { vec!["node".into()] }
        async fn open(&self, _peer: &str) -> Result<Channel> {
            let (near, far) = tokio::io::duplex(1024);
            self.far.lock().unwrap().push(far);
            let (reader, writer) = tokio::io::split(near);
            Ok(Channel { reader: Box::new(reader), writer: Box::new(writer) })
        }
    }

    fn spec(peer: &str) -> SandboxSpec {
        SandboxSpec { name: "run".into(), workspace: "/tmp".into(), env: vec![], options: SandboxOptions { peer: Some(peer.into()), ..Default::default() } }
    }

    #[tokio::test]
    async fn forwards_bytes_over_the_link() {
        let link = Arc::new(Duplex { far: Mutex::new(Vec::new()) });
        let provider = PeerProvider::new(link.clone());
        let mut sandbox = provider.start(&spec("node")).await.unwrap();
        assert_eq!(sandbox.workspace_mode(), WorkspaceMode::Copied);
        let mut channel = sandbox.take_channel().unwrap();
        let mut far = link.far.lock().unwrap().pop().unwrap();
        channel.writer.write_all(b"ping").await.unwrap();
        let mut buf = [0; 4];
        far.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
        far.write_all(b"pong").await.unwrap();
        channel.reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"pong");
        sandbox.stop().await.unwrap();
    }

    #[tokio::test]
    async fn refuses_peers_without_a_grant() {
        let provider = PeerProvider::new(Arc::new(Duplex { far: Mutex::new(Vec::new()) }));
        let error = provider.start(&spec("stranger")).await.err().unwrap();
        assert!(error.to_string().contains("no sandbox grant from peer stranger"), "{error}");
    }

    #[tokio::test]
    async fn serve_stub_pipes_a_process_in_an_empty_directory() {
        // A script stands in for the stub: it reports its directory and
        // environment, then echoes what the host sends.
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("stub");
        loa_exec::write_executable(&fake, "#!/bin/sh\nls -A \"$1\"\nprintf '[%s]' \"${HOME-}\"\nexec cat\n").unwrap();
        let (near, far) = tokio::io::duplex(1024);
        let (reader, writer) = tokio::io::split(far);
        let server = tokio::spawn(async move {
            serve_stub(Channel { reader: Box::new(reader), writer: Box::new(writer) }, &fake).await
        });
        let (mut reader, mut writer) = tokio::io::split(near);
        writer.write_all(b"hello").await.unwrap();
        writer.shutdown().await.unwrap();
        let mut echoed = String::new();
        reader.read_to_string(&mut echoed).await.unwrap();
        assert_eq!(echoed, "[]hello", "the stub starts in an empty directory with no environment");
        server.await.unwrap().unwrap();
    }
}
