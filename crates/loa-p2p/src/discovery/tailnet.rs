//! The tailnet adapter: `tailscale status --json` names candidate machines;
//! only those answering the authenticated app probe become nearby nodes.

use loa_util::exec::NoWindow;
use super::{Availability, Discoverer, NearbyPeer, NearbySource};
use crate::{NodeError, NodeId, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::net::SocketAddr;
use std::path::PathBuf;
use tokio::process::Command;

/// Port: proves which node answers at an address; a bare TCP connect never does.
#[async_trait]
pub trait NodeProbe: Send + Sync {
    async fn probe(&self, address: SocketAddr) -> Option<NodeId>;
}

/// One scan of the tailnet, yielding the machines that answered.
pub struct Tailnet<P> {
    executable: PathBuf,
    port: u16,
    probe: P,
    found: Option<Result<Vec<NearbyPeer>, String>>,
}

impl<P: NodeProbe> Tailnet<P> {
    pub fn new(executable: PathBuf, port: u16, probe: P) -> Self { Self { executable, port, probe, found: None } }

    async fn scan(&self) -> Result<Vec<NearbyPeer>> {
        let output = Command::new(loa_util::exec::resolve(&self.executable)).no_window().args(["status", "--json"]).output().await?;
        if !output.status.success() {
            return Err(NodeError::Net(format!("tailscale status exited with {}: {}", output.status, String::from_utf8_lossy(&output.stderr).trim())));
        }
        let mut candidates = Vec::new();
        for name in parse_status(&output.stdout)? {
            let addresses = tokio::net::lookup_host((name.as_str(), self.port)).await.map(Iterator::collect).unwrap_or_default();
            candidates.push((name, addresses));
        }
        Ok(admit(candidates, &self.probe).await)
    }
}

/// The first address of each candidate that the probe proves, as a nearby node.
async fn admit(candidates: Vec<(String, Vec<SocketAddr>)>, probe: &dyn NodeProbe) -> Vec<NearbyPeer> {
    let mut found = Vec::new();
    for (name, addresses) in candidates {
        for address in addresses {
            if let Some(key) = probe.probe(address).await {
                let short = name.split('.').next().unwrap_or(&name).to_owned();
                found.push(NearbyPeer::new(key, NearbySource::Tailscale, Some(short), vec![address]));
                break;
            }
        }
    }
    found
}

/// The DNS names of the peers not known to be offline; malformed names are skipped.
pub fn parse_status(json: &[u8]) -> Result<Vec<String>> {
    let root: Value = serde_json::from_slice(json).map_err(|e| NodeError::Net(format!("invalid tailscale status: {e}")))?;
    let peers = root.get("Peer").and_then(Value::as_object).ok_or_else(|| NodeError::Net("tailscale status has no Peer object".into()))?;
    let mut names: Vec<String> = peers
        .values()
        .filter(|peer| peer.get("Online").and_then(Value::as_bool) != Some(false))
        .filter_map(|peer| peer.get("DNSName").and_then(Value::as_str).and_then(dns_name))
        .collect();
    names.sort();
    names.dedup();
    Ok(names)
}

fn dns_name(raw: &str) -> Option<String> {
    let name = raw.strip_suffix('.').unwrap_or(raw);
    let label_ok = |l: &str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-');
    (name.len() <= 253 && name.contains('.') && name.split('.').all(label_ok)).then(|| name.to_ascii_lowercase())
}

#[async_trait]
impl<P: NodeProbe> Discoverer for Tailnet<P> {
    async fn next(&mut self) -> Option<NearbyPeer> {
        if self.found.is_none() {
            self.found = Some(self.scan().await.map_err(|e| e.to_string()));
        }
        self.found.as_mut()?.as_mut().ok()?.pop()
    }

    fn availability(&self) -> Availability {
        match &self.found {
            Some(Err(error)) => Availability::Failed(error.clone()),
            _ => Availability::Active("tailnet scanned".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Option<NodeId>, std::sync::Mutex<Vec<SocketAddr>>);

    #[async_trait]
    impl NodeProbe for Fake {
        async fn probe(&self, address: SocketAddr) -> Option<NodeId> {
            self.1.lock().unwrap().push(address);
            self.0
        }
    }

    #[test]
    fn status_names_are_parsed_defensively() {
        let json = br#"{"Peer":{"a":{"DNSName":"Host.Example.ts.net.","Online":true},"b":{"DNSName":"bad name.ts.net"},"c":{"HostName":"x"},"d":{"DNSName":"off.ts.net","Online":false}}}"#;
        assert_eq!(parse_status(json).unwrap(), ["host.example.ts.net"]);
        assert!(parse_status(br#"{"Peer":[]}"#).is_err() && parse_status(b"nope").is_err());
    }

    #[tokio::test]
    async fn only_probed_candidates_are_admitted() {
        let addresses = vec!["127.0.0.1:43210".parse().unwrap(), "127.0.0.2:43210".parse().unwrap()];
        let candidates = vec![("box.example.ts.net".to_owned(), addresses.clone())];
        let denied = Fake(None, Default::default());
        assert!(admit(candidates.clone(), &denied).await.is_empty());
        assert_eq!(*denied.1.lock().unwrap(), addresses, "every address was probed");
        let key = iroh::SecretKey::from_bytes(&[3; 32]).public();
        let found = admit(candidates, &Fake(Some(key), Default::default())).await;
        assert_eq!((found[0].key, found[0].name.as_deref(), &found[0].addresses[..]), (key, Some("box"), &addresses[..1]));
    }
}
