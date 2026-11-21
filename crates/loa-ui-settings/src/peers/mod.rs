//! Peers: this node, its peers and the nodes seen nearby, grants given and received (with the
//! form issuing one), and the sessions and folders peers share with this node. The app supplies
//! a [`Data`] snapshot and carries out the [`Event`]s; tokens, keys and the network stay there.

mod grant_form;
mod state;
mod view;

pub use grant_form::{Grant, GrantDirection, GrantForm, GrantFormError, GrantInput, GrantResource, GrantScope, Permission, ResourceChoice, grant_summary};
pub use state::{Confirm, Local, Message, State};

use std::cmp::Ordering;

loa_util::str_enum! {
    /// How the app found a node.
    pub Discovery { Lan => "LAN (mDNS)", Tailscale => "Tailscale", AddedByKey => "Added by key", Incoming => "Connected to this node" }
}

loa_util::str_enum! {
    pub Presence { Online => "online", Offline => "offline" }
}

loa_util::str_enum! {
    /// The route of the connection in use; `None` when there is none.
    pub Transport { Direct => "direct", Relay => "relay", TcpFallback => "TCP fallback", None => "disconnected" }
}

loa_util::str_enum! {
    /// A peer's app version against this one; protocol compatibility is reported apart.
    pub VersionRelation { Behind => "behind", Ahead => "ahead", Current => "current", Unknown => "version unknown" }
}

loa_util::str_enum! {
    #[derive(Default)]
    pub PeerSort { #[default] Name => "Name", OnlineFirst => "Online first", RecentlySeen => "Recently seen", Version => "App version" }
}

/// A peer, or a node seen nearby.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub key: String,
    pub name: String,
    pub discovery: Discovery,
    pub presence: Presence,
    pub last_seen: Option<u64>,
    pub transport: Transport,
    pub app_version: String,
    /// Whether a protocol is shared; `None` until the app talked to the node.
    pub shared_protocol: Option<bool>,
}

impl Peer {
    /// A recognizable prefix of the key; copying takes the whole key.
    pub fn short_key(&self) -> &str { self.key.char_indices().nth(12).map_or(self.key.as_str(), |(i, _)| &self.key[..i]) }

    /// Compares major, minor and patch, ignoring pre-release and build parts.
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

    /// When the peer was last seen, against the app's clock `now` (seconds).
    pub fn last_seen_text(&self, now: u64) -> String {
        match (self.presence, self.last_seen) {
            (Presence::Online, _) => "now".to_owned(),
            (_, None) => "never".to_owned(),
            (_, Some(seen)) => format!("{} ago", grant_form::duration(now.saturating_sub(seen), false)),
        }
    }
}

fn version_parts(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.trim_start_matches('v').split(['.', '-', '+']).map(str::parse);
    Some((parts.next()?.ok()?, parts.next()?.ok()?, parts.next()?.ok()?))
}

/// `peers` matching `filter` by name, key or discovery, sorted by `sort` then name and key.
pub(crate) fn visible<'a>(peers: &'a [Peer], filter: &str, sort: PeerSort) -> Vec<&'a Peer> {
    let term = filter.trim().to_lowercase();
    let mut visible: Vec<_> = peers.iter().filter(|p| [p.name.as_str(), &p.key, p.discovery.as_str()].iter().any(|s| s.to_lowercase().contains(&term))).collect();
    visible.sort_by(|a, b| {
        let order = match sort {
            PeerSort::Name => Ordering::Equal,
            PeerSort::OnlineFirst => (a.presence != Presence::Online).cmp(&(b.presence != Presence::Online)),
            PeerSort::RecentlySeen => b.last_seen.cmp(&a.last_seen),
            PeerSort::Version => version_parts(&b.app_version).cmp(&version_parts(&a.app_version)),
        };
        order.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.key.cmp(&b.key))
    });
    visible
}

/// This node as the panel shows it.
#[derive(Debug, Clone, Default)]
pub struct NodeCard {
    pub key: String,
    pub name: String,
    /// Where the node key lives, e.g. "OS keychain".
    pub key_source: String,
    /// The endpoint's state, e.g. "UDP 0.0.0.0:31415 · TCP 0.0.0.0:31415".
    pub endpoint: String,
    pub endpoint_ok: bool,
    /// LAN (mDNS) discovery state.
    pub lan: String,
    pub lan_ok: bool,
    pub tailscale: String,
    pub tailscale_ok: bool,
    /// The tailnet can be scanned now.
    pub tailscale_available: bool,
}

/// A peer's session this node's grants reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteItem {
    pub peer_key: String,
    pub peer_name: String,
    pub session: String,
    /// Title, or the session id when the grant hides it.
    pub title: String,
    pub harness: String,
    /// What the grants allow, e.g. "view, drive".
    pub access: String,
    pub can_drive: bool,
    pub can_fork: bool,
    pub online: bool,
}

/// A peer's folder this node's grants reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFolderItem {
    pub peer_key: String,
    pub peer_name: String,
    /// The folder's path on the peer.
    pub path: String,
    pub can_write: bool,
    pub online: bool,
}

/// The app's snapshot the panel shows.
#[derive(Debug, Clone, Default)]
pub struct Data {
    pub node: NodeCard,
    pub peers: Vec<Peer>,
    pub nearby: Vec<Peer>,
    pub grants: Vec<Grant>,
    pub resources: Vec<ResourceChoice>,
    pub remote_sessions: Vec<RemoteItem>,
    pub shared_folders: Vec<SharedFolderItem>,
    pub local_version: String,
    /// Current UTC instant in seconds; the app refreshes it to update countdowns.
    pub now: u64,
}

/// A request for the app's peer, clipboard or grant services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    ValidateKey(String),
    AddPeer { key: String, address: Option<String> },
    AddDiscovered(String),
    CopyKey(String),
    RemovePeer(String),
    RenamePeer { key: String, name: String },
    CreateGrant(GrantInput),
    RevokeGrant(String),
    /// Extend a given grant by its original duration and resend it.
    RenewGrant(String),
    /// Probe every peer and refresh what they share.
    Refresh,
    ScanTailscale,
    /// Open a peer's session in the center view.
    OpenRemote { peer: String, session: String },
    /// Copy a peer's session into a new session on this node.
    ForkRemote { peer: String, session: String },
    /// Browse a peer's shared folder in the center view.
    BrowseFolder { peer: String, folder: String },
}
