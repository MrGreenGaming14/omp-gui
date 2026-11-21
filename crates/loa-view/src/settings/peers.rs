//! The Peers panel: this node, its peers and the nodes seen nearby, grants given and received,
//! and the sessions and folders peers share with this node. Tokens, keys and the network stay
//! with the app.

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

    /// Major, minor and patch of the app version, ignoring a `v` prefix, pre-release and build parts.
    pub fn version(&self) -> Option<(u64, u64, u64)> { version_parts(&self.app_version) }

    /// Compares major, minor and patch with `local`, ignoring pre-release and build parts.
    pub fn version_relation(&self, local: &str) -> VersionRelation {
        match (self.version(), version_parts(local)) {
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
            (_, Some(seen)) => format!("{} ago", duration(now.saturating_sub(seen), false)),
        }
    }
}

fn version_parts(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.trim_start_matches('v').split(['.', '-', '+']).map(str::parse);
    Some((parts.next()?.ok()?, parts.next()?.ok()?, parts.next()?.ok()?))
}

/// "45s", "5m", "3h", "2d": rounded `up` for countdowns, down for ages.
fn duration(seconds: u64, up: bool) -> String {
    let (unit, name) = match seconds {
        0..60 => return format!("{seconds}s"),
        60..3600 => (60, "m"),
        3600..86400 => (3600, "h"),
        _ => (86400, "d"),
    };
    format!("{}{name}", if up { seconds.div_ceil(unit) } else { seconds / unit })
}

/// This node as the panel shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

/// What a grant protects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantScope { Session, Prompt, Project, FutureSessions, SshHost, Surface, Sandbox, Folder }

/// A resource the app offers to grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceChoice {
    pub id: String,
    pub label: String,
    pub scope: GrantScope,
}

/// A granted resource; a prompt can also cover every future session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantResource {
    pub id: String,
    pub label: String,
    pub scope: GrantScope,
    pub future_sessions: bool,
}

loa_util::str_enum! {
    /// What the holder may do with the resource.
    #[derive(Default)]
    pub Permission { View => "view", Suggest => "suggest", Drive => "drive", #[default] Fork => "fork" }
}

impl Permission {
    /// Surfaces are viewed or driven (input); SSH hosts and folders read (view) or also
    /// written, and on SSH run (drive); a sandbox only runs tasks (drive).
    pub fn allowed_for(self, scope: &GrantScope) -> bool {
        match scope {
            GrantScope::Sandbox => self == Self::Drive,
            GrantScope::Surface | GrantScope::SshHost | GrantScope::Folder => matches!(self, Self::View | Self::Drive),
            _ => true,
        }
    }
}

loa_util::str_enum! {
    /// Whether this node issued or received the grant.
    pub GrantDirection { Given => "Given", Received => "Received" }
}

/// A grant between this node and one peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub id: String,
    pub peer_key: String,
    pub direction: GrantDirection,
    pub resource: GrantResource,
    pub permission: Permission,
    /// When the grant ends; `None` renews until revoked.
    pub expires_at: Option<u64>,
    /// The next automatic renewal, if known.
    pub renews_at: Option<u64>,
    pub reshare: bool,
    /// Revoked by this node, or refused renewal by its issuer.
    pub ended: bool,
}

impl Grant {
    /// The expiry and the time to the next renewal against the app's clock `now` (seconds).
    pub fn expiry_text(&self, now: u64) -> String {
        if self.ended {
            return if self.direction == GrantDirection::Given { "revoked" } else { "revoked by its issuer" }.to_owned();
        }
        let expiry = match self.expires_at {
            None => "no end date".to_owned(),
            Some(at) if at <= now => "expired".to_owned(),
            Some(at) => format!("expires in {}", duration(at - now, true)),
        };
        match self.renews_at {
            Some(at) if at > now => format!("{expiry}; renews in {}", duration(at - now, true)),
            Some(_) => format!("{expiry}; renewal due"),
            None => expiry,
        }
    }
}

/// A validated request for the app's grant issuer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantInput {
    pub peer_key: String,
    pub resource: GrantResource,
    pub permission: Permission,
    pub ttl_seconds: u64,
    pub reshare: bool,
}

/// The app's snapshot the panel shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
pub enum Intent {
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
