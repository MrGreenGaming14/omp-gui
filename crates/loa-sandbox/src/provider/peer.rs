//! Runs the stub on a peer node. The host opens an authenticated stream to
//! the peer through a [`PeerLink`] (the app backs it with the node and the
//! peer's grant); the peer answers it with [`serve_stub`], which runs the stub
//! in a fresh, empty directory. The workspace is copied in and out over the
//! same stream (see [`crate::sync`]).

use super::{Availability, Channel, Machine, ProviderFactory, ProviderStatus, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode, status};
use crate::service::SandboxConfig;
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
    fn running(&self) -> bool { true }
    /// Names or keys of the peers that can run a sandbox now.
    fn peers(&self) -> Vec<String>;
    /// What the user calls `peer`, when it has a name besides its key.
    fn name(&self, _peer: &str) -> Option<String> { None }
    /// Opens a stream to the peer's [`serve_stub`].
    async fn open(&self, peer: &str) -> Result<Channel>;
}

pub struct PeerProvider {
    pub link: Arc<dyn PeerLink>,
}

#[async_trait]
impl SandboxProvider for PeerProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Peer }
    fn describe(&self) -> String {
        let link = &self.link;
        let peers: Vec<String> = link.peers().into_iter().map(|p| link.name(&p).map_or_else(|| p.clone(), |name| format!("{p} ({name})"))).collect();
        match peers.as_slice() {
            [] => "a peer node that granted sandbox access (none right now); set options.peer".into(),
            peers => format!("a peer node; set options.peer to one of {}", peers.join(", ")),
        }
    }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let failed = |message: String| SandboxError::Provider { provider: "peer", message };
        let peer = spec.options.peer.as_deref().ok_or_else(|| failed("peer is required".into()))?;
        if !self.link.peers().iter().any(|known| known == peer) {
            return Err(failed(format!("no sandbox grant from peer {peer}")));
        }
        Ok(Box::new(PeerSandbox(Some(self.link.open(peer).await?))))
    }
}

struct PeerSandbox(Option<Channel>);

#[async_trait]
impl Sandbox for PeerSandbox {
    fn take_channel(&mut self) -> Option<Channel> { self.0.take() }
    fn workspace_mode(&self) -> WorkspaceMode { WorkspaceMode::Copied }
    async fn stop(self: Box<Self>) -> Result<()> { Ok(()) }
}

/// The peer side of a sandbox stream: runs `stub` with an empty environment
/// in a new temporary directory and pipes the stream to it until either side
/// ends. The directory is removed afterwards.
pub async fn serve_stub(channel: Channel, stub: &Path) -> Result<()> {
    let root = tempfile::tempdir()?;
    let mut child = Command::new(stub).arg(root.path()).current_dir(root.path()).env_clear()
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn()?;
    let (mut stdin, mut stdout) = (child.stdin.take().expect("piped stdin"), child.stdout.take().expect("piped stdout"));
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

pub struct Factory;

#[async_trait]
impl ProviderFactory for Factory {
    fn kind(&self) -> SandboxKind { SandboxKind::Peer }
    async fn probe(&self, machine: &Machine, _: &SandboxConfig) -> ProviderStatus {
        // The user knows peers by name; the key stands in for an unnamed one.
        let link = machine.peers.as_ref().filter(|link| link.running());
        match link.map(|link| link.peers().into_iter().map(|peer| link.name(&peer).unwrap_or(peer)).collect::<Vec<_>>()) {
            None => status(self.kind(), Availability::Unavailable, "Peer networking is not running"),
            Some(peers) if peers.is_empty() => status(self.kind(), Availability::NotConfigured, "No peer has granted this device sandbox access"),
            Some(peers) => status(self.kind(), Availability::Ready, peers.join(", ")),
        }
    }
    fn build(&self, machine: &Machine, _: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>> {
        let link = machine.peers.as_ref().filter(|link| link.running())?;
        Some(Arc::new(PeerProvider { link: Arc::clone(link) }))
    }
}
