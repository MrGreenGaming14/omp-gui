//! Tailscale supplies candidate MagicDNS names, never app identity. Only an
//! authenticated application probe may turn a candidate into a discovered node.

use crate::NodeId;
use crate::error::{NetError, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::net::SocketAddr;
use std::path::Path;
use tokio::net::lookup_host;
use tokio::process::Command;

pub const DEFAULT_PORT: u16 = 31415;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailnetPeer {
    pub name: String,
    pub online: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPeer {
    pub name: String,
    pub address: SocketAddr,
    /// Key authenticated by the app protocol, not by Tailscale status.
    pub node: NodeId,
}

/// Probe a resolved candidate using the app's authenticated transport, e.g.
/// the iroh control handshake. A mere successful TCP connection MUST NOT return
/// a node key. `None` means the machine does not speak the app protocol.
#[async_trait]
pub trait NodeProbe: Send + Sync {
    async fn probe(&self, address: SocketAddr) -> Result<Option<NodeId>>;
}

/// Runs the given executable as `tailscale status --json`; never invokes a shell.
pub async fn status(executable: &Path) -> Result<Vec<TailnetPeer>> {
    let output = Command::new(executable)
        .args(["status", "--json"])
        .output()
        .await
        .map_err(|err| NetError::Tailscale(format!("running {}: {err}", executable.display())))?;
    if !output.status.success() {
        return Err(NetError::Tailscale(format!(
            "{} status --json exited with {}: {}",
            executable.display(), output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    parse_status(&output.stdout)
}

/// Parses peer entries defensively: missing, malformed or unusable DNS names
/// are skipped, rather than accidentally probing arbitrary shell/URL strings.
pub fn parse_status(json: &[u8]) -> Result<Vec<TailnetPeer>> {
    let root: Value = serde_json::from_slice(json)
        .map_err(|err| NetError::Tailscale(format!("invalid status JSON: {err}")))?;
    let peers = root.get("Peer").and_then(Value::as_object)
        .ok_or_else(|| NetError::Tailscale("status JSON has no Peer object".into()))?;
    let mut result = Vec::new();
    for peer in peers.values() {
        let Some(name) = peer.get("DNSName").and_then(Value::as_str).and_then(normalize_dns_name) else {
            continue;
        };
        let online = peer.get("Online").and_then(Value::as_bool);
        if let Some(existing) = result.iter_mut().find(|entry: &&mut TailnetPeer| entry.name == name) {
            existing.online = match (existing.online, online) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), _) | (_, Some(false)) => Some(false),
                _ => None,
            };
        } else {
            result.push(TailnetPeer { name, online });
        }
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

fn normalize_dns_name(raw: &str) -> Option<String> {
    let name = raw.strip_suffix('.').unwrap_or(raw);
    if name.len() > 253 || name.is_empty() || !name.is_ascii() || !name.contains('.')
        || name.split('.').any(|label| {
            label.is_empty() || label.len() > 63
                || label.starts_with('-') || label.ends_with('-')
                || !label.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return None;
    }
    Some(name.to_ascii_lowercase())
}

/// Resolves MagicDNS names on `port` (default 31415 when `None`) and admits
/// only candidates that answer the caller's authenticated app protocol probe.
/// A failed DNS resolution or rejected address does not hide other peers.
pub async fn discover(peers: &[TailnetPeer], port: Option<u16>, probe: &dyn NodeProbe) -> Vec<AppPeer> {
    let port = port.unwrap_or(DEFAULT_PORT);
    if port == 0 {
        return Vec::new();
    }
    let mut found = Vec::new();
    for peer in peers {
        if peer.online == Some(false) {
            continue;
        }
        let Some(name) = normalize_dns_name(&peer.name) else { continue };
        let Ok(addresses) = lookup_host((name.as_str(), port)).await else { continue };
        for address in addresses {
            if let Ok(Some(node)) = probe.probe(address).await {
                found.push(AppPeer { name: name.clone(), address, node });
                break;
            }
        }
    }
    found
}

/// Executes status discovery and probes each usable name.
pub async fn discover_from_status(executable: &Path, port: Option<u16>, probe: &dyn NodeProbe) -> Result<Vec<AppPeer>> {
    let peers = status(executable).await?;
    Ok(discover(&peers, port, probe).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeProbe {
        seen: Mutex<Vec<SocketAddr>>,
        answer: Option<NodeId>,
    }

    #[async_trait]
    impl NodeProbe for FakeProbe {
        async fn probe(&self, address: SocketAddr) -> Result<Option<NodeId>> {
            self.seen.lock().unwrap().push(address);
            Ok(self.answer)
        }
    }

    #[test]
    fn parses_names_and_skips_malformed_entries() {
        let peers = parse_status(br#"{"Peer":{"one":{"DNSName":"Host.Example.ts.net.","Online":true},"two":{"DNSName":"bad name.ts.net"},"three":{"HostName":"no-dns"},"four":{"DNSName":"host.example.ts.net."}}}"#).unwrap();
        assert_eq!(peers, vec![TailnetPeer { name: "host.example.ts.net".into(), online: Some(true) }]);
        assert!(parse_status(br#"{"Peer":[]}"#).is_err());
        assert!(parse_status(b"not-json").is_err());
    }

    #[tokio::test]
    async fn probe_must_confirm_app_identity() {
        let candidate = TailnetPeer { name: "localhost.localhost".into(), online: Some(true) };
        // The localhost name can be resolved without querying any real tailnet.
        let candidate = TailnetPeer { name: "localhost".into(), ..candidate };
        // `discover` accepts only fully qualified names; test with a DNS name
        // resolved from the process hosts file on machines with localhost.localdomain.
        let candidate = TailnetPeer { name: "localhost.localdomain".into(), ..candidate };
        let denied = FakeProbe { seen: Mutex::new(Vec::new()), answer: None };
        let result = discover(std::slice::from_ref(&candidate), Some(43210), &denied).await;
        assert!(result.is_empty());
        if !denied.seen.lock().unwrap().is_empty() {
            let key = iroh::SecretKey::from_bytes(&[3; 32]).public();
            let accepted = FakeProbe { seen: Mutex::new(Vec::new()), answer: Some(key) };
            let result = discover(&[candidate], Some(43210), &accepted).await;
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].node, key);
            assert_eq!(result[0].address.port(), 43210);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fake_executable_supplies_status_without_tailnet() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("tailscale fake");
        std::fs::write(&executable, "#!/bin/sh\n[ \"$1\" = status ] && [ \"$2\" = --json ] || exit 2\nprintf '%s' '{\"Peer\":{\"one\":{\"DNSName\":\"node.example.ts.net.\",\"Online\":true}}}'\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(status(&executable).await.unwrap(), vec![TailnetPeer { name: "node.example.ts.net".into(), online: Some(true) }]);
        let probe = FakeProbe { seen: Mutex::new(Vec::new()), answer: None };
        assert!(discover_from_status(&executable, None, &probe).await.unwrap().is_empty());
    }
}
