//! Peer presentation and filtering without transport dependencies.

use std::cmp::Ordering;
use std::fmt;

/// How the app found this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discovery { Lan, Tailscale, AddedByKey, Incoming }
impl fmt::Display for Discovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self { Self::Lan => "LAN (mDNS)", Self::Tailscale => "Tailscale", Self::AddedByKey => "Added by key", Self::Incoming => "Connected to this node" })
    }
}

/// Presence reported by the app's network service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence { Online, Offline }
impl fmt::Display for Presence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self { Self::Online => "online", Self::Offline => "offline" })
    }
}

/// Active connection route; None means no connection is currently in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport { Direct, Relay, TcpFallback, None }
impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self { Self::Direct => "direct", Self::Relay => "relay", Self::TcpFallback => "TCP fallback", Self::None => "disconnected" })
    }
}

/// A peer or newly discovered node supplied by the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub key: String,
    pub name: String,
    pub discovery: Discovery,
    pub presence: Presence,
    pub last_seen: Option<u64>,
    pub transport: Transport,
    pub app_version: String,
    /// Whether at least one ALPN version is shared, as negotiated by the app;
    /// `None` until the app has talked to the node.
    pub shared_protocol: Option<bool>,
}

/// Relative app version; compatibility is reported separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionRelation { Behind, Ahead, Current, Unknown }
impl fmt::Display for VersionRelation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self { Self::Behind => "behind", Self::Ahead => "ahead", Self::Current => "current", Self::Unknown => "version unknown" })
    }
}

impl Peer {
    /// Show a recognizable prefix while retaining the full key for copying.
    pub fn short_key(&self) -> &str {
        let end = self.key.char_indices().nth(12).map_or(self.key.len(), |(i, _)| i);
        &self.key[..end]
    }

    /// Compare the numeric major/minor/patch components, ignoring build metadata.
    pub fn version_relation(&self, local: &str) -> VersionRelation {
        match (version_parts(&self.app_version), version_parts(local)) {
            (Some(remote), Some(local)) => match remote.cmp(&local) {
                Ordering::Less => VersionRelation::Behind,
                Ordering::Equal => VersionRelation::Current,
                Ordering::Greater => VersionRelation::Ahead,
            },
            _ => VersionRelation::Unknown,
        }
    }

    /// Render the last known presence relative to the app-supplied clock.
    pub fn last_seen_text(&self, now: u64) -> String {
        if self.presence == Presence::Online { return "now".to_owned(); }
        let Some(seen) = self.last_seen else { return "never".to_owned(); };
        let age = now.saturating_sub(seen);
        if age < 60 { format!("{age}s ago") }
        else if age < 3600 { format!("{}m ago", age / 60) }
        else if age < 86400 { format!("{}h ago", age / 3600) }
        else { format!("{}d ago", age / 86400) }
    }
}

fn version_parts(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.trim_start_matches('v').split(['.', '-', '+']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Peer sort selected by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PeerSort { #[default] Name, OnlineFirst, RecentlySeen, Version }
impl PeerSort {
    pub const ALL: [Self; 4] = [Self::Name, Self::OnlineFirst, Self::RecentlySeen, Self::Version];
}
impl fmt::Display for PeerSort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self { Self::Name => "Name", Self::OnlineFirst => "Online first", Self::RecentlySeen => "Recently seen", Self::Version => "App version" })
    }
}

pub(crate) fn visible_peers<'a>(peers: &'a [Peer], filter: &str, sort: PeerSort) -> Vec<&'a Peer> {
    let term = filter.trim().to_lowercase();
    let mut visible: Vec<_> = peers.iter().filter(|peer| {
        term.is_empty() || peer.name.to_lowercase().contains(&term)
            || peer.key.to_lowercase().contains(&term)
            || peer.discovery.to_string().to_lowercase().contains(&term)
    }).collect();
    visible.sort_by(|a, b| {
        let order = match sort {
            PeerSort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            PeerSort::OnlineFirst => (a.presence != Presence::Online).cmp(&(b.presence != Presence::Online)),
            PeerSort::RecentlySeen => b.last_seen.cmp(&a.last_seen),
            PeerSort::Version => version_parts(&b.app_version).cmp(&version_parts(&a.app_version)),
        };
        order.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.key.cmp(&b.key))
    });
    visible
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(key: &str, name: &str, presence: Presence, last_seen: Option<u64>, version: &str) -> Peer {
        Peer { key: key.into(), name: name.into(), discovery: Discovery::Lan, presence, last_seen, transport: Transport::None, app_version: version.into(), shared_protocol: Some(true) }
    }

    #[test]
    fn filter_and_sort_peers_by_presence_recently_seen_and_name() {
        let peers = vec![peer("z", "Zed", Presence::Offline, Some(20), "1.0.0"), peer("a", "Alice", Presence::Online, Some(10), "2.0.0"), peer("b", "Bob", Presence::Offline, Some(30), "1.5.0")];
        assert_eq!(visible_peers(&peers, "ALi", PeerSort::Name).iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), vec!["a"]);
        assert_eq!(visible_peers(&peers, "", PeerSort::OnlineFirst).iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), vec!["a", "b", "z"]);
        assert_eq!(visible_peers(&peers, "", PeerSort::RecentlySeen).iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), vec!["b", "z", "a"]);
        assert_eq!(visible_peers(&peers, "", PeerSort::Version).iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), vec!["a", "b", "z"]);
    }

    #[test]
    fn app_version_marker_is_independent_of_protocol_compatibility() {
        let mut node = peer("a", "Alice", Presence::Online, None, "v1.2.3");
        node.shared_protocol = Some(false);
        assert_eq!(node.version_relation("1.4.0"), VersionRelation::Behind);
        node.app_version = "1.5.0".into();
        assert_eq!(node.version_relation("1.4.0"), VersionRelation::Ahead);
        assert_eq!(node.shared_protocol, Some(false));
    }
}
