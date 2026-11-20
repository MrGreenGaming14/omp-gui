//! The app's peer node: one service that owns this node's network presence.
//!
//! - the iroh endpoint on 31415 (UDP) with the authenticated TCP fallback on
//!   the same port, both keyed by the node identity ([`Node::start`])
//! - peers: added by key, found on the LAN (mDNS) or the tailnet, probed for
//!   presence, transport and app version ([`Node::peers`], [`Node::nearby`])
//! - grants: issued, renewed, revoked and delivered over `omp/control/1`;
//!   received grants renew themselves before they expire ([`Node::grants`])
//! - session sharing over `omp/session/1`: peers list, view and drive the
//!   sessions their grants reach; this node stays the single writer
//!   ([`Node::open_remote`], [`RemoteView`], [`Node::fork_remote`])
//!
//! The app plugs in its sessions through [`LocalSessions`] and opens every
//! session writer with [`Node::authorizer`], so peer requests are checked
//! against grants under the writer lock. Every incoming connection is
//! authenticated by its node key; control traffic is open to any node (a
//! connection never makes it a peer), session traffic needs grants proven on
//! each request.

mod access;
mod control;
mod extra;
mod folder;
mod sessions;
mod share;
pub mod ui;

pub use access::GrantAccess;
pub use extra::{AddedProtocol, NodeSlot, PeerProtocol};
pub use folder::{FolderEntry, MAX_FILE as MAX_FOLDER_FILE, SharedFolder};
pub use loa_net::conn::Stream;
pub use sessions::{BusSessions, LibrarySessions, LocalSession, LocalSessions, LocalTurn};
pub use share::{DriveOutcome, RemoteView};

pub use loa_identity::KeySource;
pub use loa_net::NodeId;
pub use loa_net::conn::Transport;
pub use loa_net::endpoint::RelayConfig;
pub use loa_net::peers::DiscoverySource;

use loa_core::now_ms;
use loa_grants::{Action, GrantId, GrantSpec, GrantStore, NodePublicKey, RenewOutcome, Renewer, Resource, Scope};
use loa_identity::NodeIdentity;
use loa_journal::{Journal, Store};
use loa_net::alpn;
use loa_net::conn::Connection;
use loa_net::control::{self as wire, control_frame::Kind};
use loa_net::network::Network;
use loa_net::peers::PeerStore;
use loa_net::router::{AllowAll, Router};
use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Default port of every node, UDP and TCP.
pub const DEFAULT_PORT: u16 = loa_net::network::DEFAULT_PORT;
/// Protocol versions this build speaks.
const VERSIONS: &[u32] = &[1];
/// How the panel names [`Resource::Sandbox`].
pub const SANDBOX_LABEL: &str = "Sandbox on this node";

/// The ALPN labels this node announces in its hello, e.g. `omp/session/1`.
pub fn local_protocols() -> Vec<String> {
    local_alpns().map(|alpn| alpn.to_string()).collect()
}

fn local_alpns() -> impl Iterator<Item = alpn::Alpn> {
    [alpn::CONTROL, alpn::SESSION, folder::PROTOCOL]
        .into_iter()
        .flat_map(|name| VERSIONS.iter().map(move |v| alpn::Alpn::new(name, *v)))
}

/// How long one probe or control request may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error(transparent)]
    Net(#[from] loa_net::NetError),
    #[error(transparent)]
    Grant(#[from] loa_grants::GrantError),
    #[error(transparent)]
    Share(#[from] loa_share::Error),
    #[error("not a node key: {0:?}")]
    InvalidKey(String),
    #[error("invalid address {0:?}: {1}")]
    InvalidAddress(String, String),
    #[error("unknown peer {0}")]
    UnknownPeer(String),
    #[error("the peer endpoint is not running")]
    NotStarted,
    #[error("the peer refused: {0}")]
    Refused(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error("{0}")]
    Unavailable(String),
}

pub type Result<T, E = NodeError> = std::result::Result<T, E>;

/// How the node runs. [`NodeConfig::new`] gives the app defaults.
#[derive(Clone, Debug)]
pub struct NodeConfig {
    /// Where `peers.sqlite` and `grants.sqlite` live.
    pub data_dir: PathBuf,
    /// UDP and TCP listening address; port 0 picks free ports (tests).
    pub bind: SocketAddr,
    pub relay: RelayConfig,
    /// Announce and browse on the LAN.
    pub mdns: bool,
    /// The `tailscale` executable; `None` when Tailscale isn't installed.
    pub tailscale: Option<PathBuf>,
    /// The name peers see, the machine's host name.
    pub name: String,
    pub app_version: String,
    /// How often peers are probed and held grants renewed.
    pub presence_interval: Duration,
    /// Held grants renew this long before they expire.
    pub renew_margin_ms: i64,
    /// Where forks of peers' sessions become local sessions: the app's
    /// session store. `None` disables forking.
    pub sessions: Option<Store>,
}

impl NodeConfig {
    /// The app's defaults: port 31415 on every interface, mDNS on, Tailscale
    /// when its CLI is on `PATH`, presence every 30 s.
    pub fn new(data_dir: impl Into<PathBuf>, name: impl Into<String>, app_version: impl Into<String>) -> Self {
        Self {
            data_dir: data_dir.into(),
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_PORT)),
            relay: RelayConfig::Off,
            mdns: true,
            tailscale: find_executable("tailscale"),
            name: name.into(),
            app_version: app_version.into(),
            presence_interval: Duration::from_secs(30),
            renew_margin_ms: 3 * 60_000,
            sessions: None,
        }
    }
}

fn create_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| NodeError::Unavailable(format!("{}: {e}", dir.display())))
}

fn find_executable(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// State of the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Stopped,
    Starting,
    Listening { udp: Vec<SocketAddr>, tcp: SocketAddr },
    Failed(String),
}

/// Whether an optional discovery source works here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Turned off, or its tool isn't installed.
    Off(String),
    Active(String),
    Failed(String),
}

/// What the node is and how it is reachable.
#[derive(Clone, Debug)]
pub struct NodeStatus {
    pub key: NodeId,
    pub name: String,
    pub app_version: String,
    pub key_source: KeySource,
    pub endpoint: Endpoint,
    pub mdns: Availability,
    pub tailscale: Availability,
}

/// A saved peer with what this node last observed about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerInfo {
    pub key: NodeId,
    pub name: Option<String>,
    pub addresses: Vec<SocketAddr>,
    pub source: DiscoverySource,
    pub online: bool,
    pub last_seen_ms: Option<i64>,
    pub app_version: Option<String>,
    /// Transport of the last connection, while the peer is online.
    pub transport: Option<Transport>,
    /// Whether it speaks a version of `omp/session` this node does.
    pub shared_protocol: Option<bool>,
    /// The ALPN labels (`omp/<family>/<n>`) it announced in its last hello;
    /// empty until it has been seen.
    pub protocols: Vec<String>,
}

/// How an unsaved node became known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NearbySource {
    Mdns,
    Tailscale,
    /// It connected to this node.
    Incoming,
}

/// A node seen but not saved as a peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyPeer {
    pub key: NodeId,
    pub name: Option<String>,
    pub addresses: Vec<SocketAddr>,
    pub source: NearbySource,
    pub seen_ms: i64,
    pub app_version: Option<String>,
    /// The ALPN labels it announced, when it connected or answered a probe.
    pub protocols: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantDirection {
    Given,
    Received,
}

/// A grant this node issued or holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantInfo {
    pub id: GrantId,
    /// The holder of a given grant, the issuer of a received one.
    pub peer: NodeId,
    pub direction: GrantDirection,
    pub scopes: Vec<Scope>,
    /// When the grant ends (given grants with an end).
    pub until_ms: Option<i64>,
    /// When the current token expires (received grants).
    pub expires_ms: Option<i64>,
    /// When this node renews it (received grants).
    pub renews_at_ms: Option<i64>,
    pub reshare: bool,
    /// Revoked by this node, or refused renewal by its issuer.
    pub ended: bool,
}

/// A session of a peer that this node's grants reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSession {
    pub peer: NodeId,
    pub id: String,
    pub title: String,
    pub harness: String,
    pub created_ms: i64,
    pub actions: Vec<Action>,
    /// Only some prompts are visible.
    pub partial: bool,
    pub last_seq: u64,
}

/// Something the app should react to.
#[derive(Clone, Debug)]
pub enum NodeEvent {
    /// Peers, grants, status or remote sessions changed: refresh views.
    Changed,
    /// A peer with drive access asks this node's harness to run a prompt.
    PromptRequested { peer: NodeId, session: String, text: String },
    /// A peer with drive access answers a decision waiting in this node's
    /// session `session`; `decision` is this node's id of it.
    AnswerRequested { peer: NodeId, session: String, decision: loa_core::NodeId, values: Vec<String>, reason: Option<String> },
}

/// The running node. Cheap to clone; every clone is the same node.
#[derive(Clone)]
pub struct Node {
    inner: Arc<Inner>,
}

pub(crate) struct Inner {
    config: NodeConfig,
    seed: [u8; 32],
    key: NodeId,
    grant_key: loa_grants::NodeKey,
    peers: Mutex<PeerStore>,
    grants: Arc<GrantStore>,
    access: Arc<GrantAccess>,
    sessions: Arc<dyn LocalSessions>,
    net: Mutex<Option<Arc<Network>>>,
    state: Mutex<State>,
    events: broadcast::Sender<NodeEvent>,
}

struct State {
    key_source: KeySource,
    endpoint: Endpoint,
    mdns: Availability,
    tailscale: Availability,
    transports: HashMap<NodeId, Transport>,
    /// ALPN labels each peer announced in its last hello.
    protocols: HashMap<NodeId, Vec<String>>,
    nearby: HashMap<NodeId, NearbyPeer>,
    remote: HashMap<NodeId, Vec<RemoteSession>>,
    /// Grants issued while their holder was unreachable; delivered when it answers.
    undelivered: HashSet<GrantId>,
    resources: Vec<(Resource, String)>,
    /// Protocols the app added; served from the next start.
    added: Vec<AddedProtocol>,
    tasks: Vec<JoinHandle<()>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn grant_key(id: &NodeId) -> NodePublicKey {
    NodePublicKey(*id.as_bytes())
}

pub(crate) fn bus_key(id: &NodeId) -> loa_bus::NodeKey {
    loa_bus::NodeKey(*id.as_bytes())
}

fn node_id(key: &NodePublicKey) -> Option<NodeId> {
    NodeId::from_bytes(&key.0).ok()
}

impl Node {
    /// Opens the node's stores under `config.data_dir`, grants included.
    /// Nothing touches the network until [`Node::start`].
    pub fn open(config: NodeConfig, identity: &NodeIdentity, key_source: KeySource, sessions: Arc<dyn LocalSessions>) -> Result<Self> {
        create_dir(&config.data_dir)?;
        let grants = Arc::new(GrantStore::open(&config.data_dir.join("grants.sqlite"))?);
        Self::open_with_grants(config, identity, key_source, sessions, grants)
    }

    /// [`Node::open`] over the app's grant store, shared with whatever else
    /// checks grants; only the peer directory lives under `config.data_dir`.
    pub fn open_with_grants(
        config: NodeConfig,
        identity: &NodeIdentity,
        key_source: KeySource,
        sessions: Arc<dyn LocalSessions>,
        grants: Arc<GrantStore>,
    ) -> Result<Self> {
        create_dir(&config.data_dir)?;
        let peers = PeerStore::open(config.data_dir.join("peers.sqlite"))?;
        let seed = *identity.secret_bytes();
        let key = NodeId::from_bytes(identity.public_key().as_bytes()).map_err(|e| NodeError::InvalidKey(e.to_string()))?;
        let grant_key = loa_grants::NodeKey::from_secret(&seed)?;
        let mdns = if config.mdns { Availability::Off("starts with the endpoint".into()) } else { Availability::Off("turned off".into()) };
        let tailscale = match &config.tailscale {
            Some(path) => Availability::Active(format!("{} found; scan to look for nodes", path.display())),
            None if find_executable("tailscale").is_some() => Availability::Off("turned off".into()),
            None => Availability::Off("tailscale is not installed".into()),
        };
        let (events, _) = broadcast::channel(256);
        Ok(Self {
            inner: Arc::new(Inner {
                access: Arc::new(GrantAccess::new(Arc::clone(&grants))),
                config,
                seed,
                key,
                grant_key,
                peers: Mutex::new(peers),
                grants,
                sessions,
                net: Mutex::new(None),
                state: Mutex::new(State {
                    key_source,
                    endpoint: Endpoint::Stopped,
                    mdns,
                    tailscale,
                    transports: HashMap::new(),
                    protocols: HashMap::new(),
                    nearby: HashMap::new(),
                    remote: HashMap::new(),
                    undelivered: HashSet::new(),
                    resources: Vec::new(),
                    added: Vec::new(),
                    tasks: Vec::new(),
                }),
                events,
            }),
        })
    }

    pub(crate) fn from_inner(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    pub fn key(&self) -> NodeId {
        self.inner.key
    }

    pub fn config(&self) -> &NodeConfig {
        &self.inner.config
    }

    /// The `Authorize` port every session writer this node owns must use:
    /// peer requests pass only with a live grant for that change.
    pub fn authorizer(&self) -> Arc<dyn loa_bus::Authorize> {
        Arc::clone(&self.inner.access) as Arc<dyn loa_bus::Authorize>
    }

    /// Changes and requests, for the app's subscription.
    pub fn events(&self) -> broadcast::Receiver<NodeEvent> {
        self.inner.events.subscribe()
    }

    pub(crate) fn changed(&self) {
        let _ = self.inner.events.send(NodeEvent::Changed);
    }

    pub(crate) fn emit(&self, event: NodeEvent) {
        let _ = self.inner.events.send(event);
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.inner.state)
    }

    fn peers_db(&self) -> MutexGuard<'_, PeerStore> {
        lock(&self.inner.peers)
    }

    pub(crate) fn grant_store(&self) -> &Arc<GrantStore> {
        &self.inner.grants
    }

    pub(crate) fn access(&self) -> &Arc<GrantAccess> {
        &self.inner.access
    }

    pub(crate) fn local_sessions(&self) -> &Arc<dyn LocalSessions> {
        &self.inner.sessions
    }

    pub(crate) fn grant_signer(&self) -> &loa_grants::NodeKey {
        &self.inner.grant_key
    }

    pub(crate) fn network(&self) -> Result<Arc<Network>> {
        lock(&self.inner.net).clone().ok_or(NodeError::NotStarted)
    }

    /// Binds the endpoint and starts discovery, presence and renewal.
    /// Starting a running node does nothing.
    pub async fn start(&self) -> Result<()> {
        if lock(&self.inner.net).is_some() {
            return Ok(());
        }
        self.state().endpoint = Endpoint::Starting;
        self.changed();
        let weak = Arc::downgrade(&self.inner);
        // Every node may open control (that is how grants and presence
        // arrive); session requests are checked against grants per request.
        let mut router = Router::new(Arc::new(AllowAll))
            .handler(alpn::CONTROL, 1, Arc::new(control::Handler(weak.clone())))
            .handler(alpn::SESSION, 1, Arc::new(share::Handler(weak.clone())))
            .handler(folder::PROTOCOL, 1, Arc::new(folder::Handler(weak.clone())));
        for protocol in self.added_protocols() {
            let (name, version) = (protocol.name, protocol.version);
            router = router.handler(name, version, Arc::new(extra::Handler { inner: weak.clone(), protocol }));
        }
        let config = &self.inner.config;
        let net = match Network::bind(self.inner.seed, config.bind, config.relay.clone(), router).await {
            Ok(net) => Arc::new(net),
            Err(error) => {
                self.state().endpoint = Endpoint::Failed(error.to_string());
                self.changed();
                return Err(error.into());
            }
        };
        let udp = net.quic().local_addrs();
        let tcp = net.tcp().local_addr();
        *lock(&self.inner.net) = Some(Arc::clone(&net));
        let mut tasks = Vec::new();
        let mdns = if config.mdns {
            let announce = loa_net::discovery::Announce {
                node: self.inner.key,
                udp_port: udp.first().map_or(0, SocketAddr::port),
                tcp_port: tcp.port(),
                name: config.name.clone(),
            };
            match loa_net::discovery::Discovery::new(&announce, true) {
                Ok(discovery) => {
                    tasks.push(tokio::spawn(control::browse(weak.clone(), discovery)));
                    Availability::Active("announcing and browsing on the LAN".into())
                }
                Err(error) => Availability::Failed(error.to_string()),
            }
        } else {
            Availability::Off("turned off".into())
        };
        tasks.push(tokio::spawn(background(weak, config.presence_interval)));
        {
            let mut state = self.state();
            state.endpoint = Endpoint::Listening { udp, tcp };
            state.mdns = mdns;
            state.tasks = tasks;
        }
        self.changed();
        Ok(())
    }

    /// Stops discovery and closes the endpoint.
    pub async fn stop(&self) {
        let tasks = std::mem::take(&mut self.state().tasks);
        for task in tasks {
            task.abort();
        }
        let net = lock(&self.inner.net).take();
        if let Some(net) = net {
            net.close().await;
        }
        {
            let mut state = self.state();
            state.endpoint = Endpoint::Stopped;
            state.transports.clear();
            if matches!(state.mdns, Availability::Active(_)) {
                state.mdns = Availability::Off("stopped with the endpoint".into());
            }
        }
        self.changed();
    }

    pub fn status(&self) -> NodeStatus {
        let state = self.state();
        NodeStatus {
            key: self.inner.key,
            name: self.inner.config.name.clone(),
            app_version: self.inner.config.app_version.clone(),
            key_source: state.key_source.clone(),
            endpoint: state.endpoint.clone(),
            mdns: state.mdns.clone(),
            tailscale: state.tailscale.clone(),
        }
    }

    /// Parses a node key in the iroh endpoint id form (64 hex characters or
    /// 52 base32) or the Biscuit form (`ed25519/<hex>`).
    pub fn parse_key(text: &str) -> Result<NodeId> {
        let text = text.trim();
        let key = loa_identity::PublicKey::from_str(text).map_err(|_| NodeError::InvalidKey(text.to_owned()))?;
        NodeId::from_bytes(key.as_bytes()).map_err(|_| NodeError::InvalidKey(text.to_owned()))
    }

    /// Saves a peer by key, with an optional `host[:port]` to reach it, then
    /// probes it.
    pub async fn add_peer(&self, key: &str, address: Option<&str>) -> Result<()> {
        let key = Self::parse_key(key)?;
        if key == self.inner.key {
            return Err(NodeError::InvalidKey("this node's own key".into()));
        }
        let addresses = match address.map(str::trim).filter(|a| !a.is_empty()) {
            Some(address) => resolve(address).await?,
            None => self.state().nearby.get(&key).map(|n| n.addresses.clone()).unwrap_or_default(),
        };
        let name = self.state().nearby.get(&key).and_then(|n| n.name.clone());
        self.peers_db().add_peer(&key.to_string(), name.as_deref(), &addresses, DiscoverySource::Manual)?;
        self.state().nearby.remove(&key);
        self.changed();
        self.probe(key).await;
        Ok(())
    }

    /// Saves a node found on the LAN, the tailnet or that connected here.
    pub async fn add_nearby(&self, key: &str) -> Result<()> {
        let key = Self::parse_key(key)?;
        let Some(nearby) = self.state().nearby.remove(&key) else {
            return Err(NodeError::UnknownPeer(key.to_string()));
        };
        let source = match nearby.source {
            NearbySource::Mdns => DiscoverySource::Mdns,
            NearbySource::Tailscale => DiscoverySource::Tailscale,
            NearbySource::Incoming => DiscoverySource::Manual,
        };
        self.peers_db().add_peer(&key.to_string(), nearby.name.as_deref(), &nearby.addresses, source)?;
        if let Some(version) = &nearby.app_version {
            self.peers_db().update_app_version(&key, version)?;
        }
        self.changed();
        self.probe(key).await;
        Ok(())
    }

    pub fn rename_peer(&self, key: &str, name: &str) -> Result<()> {
        let key = Self::parse_key(key)?;
        if !self.peers_db().rename(&key, name.trim())? {
            return Err(NodeError::UnknownPeer(key.to_string()));
        }
        self.changed();
        Ok(())
    }

    /// Forgets a peer. Grants given to it stay until revoked.
    pub fn remove_peer(&self, key: &str) -> Result<()> {
        let key = Self::parse_key(key)?;
        if !self.peers_db().remove(&key)? {
            return Err(NodeError::UnknownPeer(key.to_string()));
        }
        {
            let mut state = self.state();
            state.transports.remove(&key);
            state.remote.remove(&key);
        }
        self.changed();
        Ok(())
    }

    pub fn peers(&self) -> Vec<PeerInfo> {
        let peers = self.peers_db().list().unwrap_or_default();
        let state = self.state();
        peers
            .into_iter()
            .map(|peer| PeerInfo {
                transport: if peer.online { state.transports.get(&peer.key).copied() } else { None },
                shared_protocol: state.protocols.get(&peer.key).map(|labels| {
                    VERSIONS.iter().any(|v| labels.contains(&alpn::Alpn::new(alpn::SESSION, *v).to_string()))
                }),
                protocols: state.protocols.get(&peer.key).cloned().unwrap_or_default(),
                key: peer.key,
                name: peer.name,
                addresses: peer.addresses,
                source: peer.discovery_source,
                online: peer.online,
                last_seen_ms: peer.last_seen_ms,
                app_version: peer.app_version,
            })
            .collect()
    }

    pub fn nearby(&self) -> Vec<NearbyPeer> {
        let mut nearby: Vec<_> = self.state().nearby.values().cloned().collect();
        nearby.sort_by(|a, b| b.seen_ms.cmp(&a.seen_ms).then_with(|| a.key.cmp(&b.key)));
        nearby
    }

    /// A peer's display name: what the user named it, its own announced name,
    /// or the start of its key.
    pub fn peer_name(&self, key: &NodeId) -> String {
        if let Ok(Some(peer)) = self.peers_db().lookup(key)
            && let Some(name) = peer.name.filter(|n| !n.is_empty())
        {
            return name;
        }
        if let Some(name) = self.state().nearby.get(key).and_then(|n| n.name.clone()) {
            return name;
        }
        key.fmt_short().to_string()
    }

    /// Probes every saved peer, refreshes the sessions they share and
    /// delivers grants that were waiting for them.
    pub async fn refresh_presence(&self) {
        let peers: Vec<NodeId> = self.peers_db().list().unwrap_or_default().into_iter().map(|p| p.key).collect();
        let mut probes = tokio::task::JoinSet::new();
        for peer in peers {
            let node = self.clone();
            probes.spawn(async move { node.probe(peer).await });
        }
        while probes.join_next().await.is_some() {}
    }

    /// One presence round trip to `peer`, then the follow-ups it unblocks.
    pub(crate) async fn probe(&self, peer: NodeId) {
        let presence = Kind::Presence(wire::Presence { online: true, last_seen_ms: Some(now_ms()) });
        match self.control_call(peer, presence).await {
            Ok(_) => {
                self.deliver_waiting(peer).await;
                if self.holds_grants_from(&peer) {
                    let _ = self.list_remote(&peer.to_string()).await;
                } else {
                    // Its grants ended: its sessions leave Recents.
                    let listed = self.state().remote.remove(&peer).is_some();
                    if listed {
                        self.changed();
                    }
                }
            }
            Err(error) => {
                tracing::debug!(%peer, %error, "peer unreachable");
                let _ = self.peers_db().update_presence(&peer, false, now_ms());
                self.state().transports.remove(&peer);
                self.changed();
            }
        }
    }

    fn holds_grants_from(&self, peer: &NodeId) -> bool {
        let root = grant_key(peer);
        self.inner.grants.received_all().unwrap_or_default().iter().any(|g| g.root == root && g.refused_ms.is_none())
    }

    /// Records what a connection showed about `peer`.
    pub(crate) fn observe(&self, peer: NodeId, hello: &wire::Hello, transport: Transport) {
        let known = {
            let db = self.peers_db();
            let known = db.update_app_version(&peer, &hello.app_version).unwrap_or(false);
            if known {
                if !hello.name.is_empty() {
                    let _ = db.name_if_unset(&peer, &hello.name);
                }
                let _ = db.update_presence(&peer, true, now_ms());
            }
            known
        };
        {
            let mut state = self.state();
            state.transports.insert(peer, transport);
            let labels: Vec<String> =
                hello.protocols.iter().map(|p| alpn::Alpn::new(p.name.as_str(), p.version).to_string()).collect();
            state.protocols.insert(peer, labels.clone());
            if !known {
                let entry = state.nearby.entry(peer).or_insert_with(|| NearbyPeer {
                    key: peer,
                    name: None,
                    addresses: Vec::new(),
                    source: NearbySource::Incoming,
                    seen_ms: 0,
                    app_version: None,
                    protocols: Vec::new(),
                });
                entry.seen_ms = now_ms();
                entry.app_version = Some(hello.app_version.clone());
                entry.protocols = labels;
                if !hello.name.is_empty() {
                    entry.name = Some(hello.name.clone());
                }
            }
        }
        self.changed();
    }

    pub(crate) fn hello(&self) -> wire::Hello {
        let protocols = local_alpns().map(|a| wire::ProtocolVersion { name: a.protocol, version: a.version }).collect();
        wire::Hello { app_version: self.inner.config.app_version.clone(), protocols, name: self.inner.config.name.clone() }
    }

    fn hints(&self, peer: &NodeId) -> Vec<SocketAddr> {
        let mut hints = self.peers_db().lookup(peer).ok().flatten().map(|p| p.addresses).unwrap_or_default();
        if let Some(nearby) = self.state().nearby.get(peer) {
            for address in &nearby.addresses {
                if !hints.contains(address) {
                    hints.push(*address);
                }
            }
        }
        hints
    }

    /// Dials `peer` on `protocol`: QUIC first, then TCP, on its address hints.
    pub(crate) async fn dial(&self, peer: NodeId, protocol: &str) -> Result<Connection> {
        self.dial_versions(peer, protocol, VERSIONS).await
    }

    /// Dials `peer` on `protocol` offering `versions`.
    pub(crate) async fn dial_versions(&self, peer: NodeId, protocol: &str, versions: &[u32]) -> Result<Connection> {
        let net = self.network()?;
        let hints = self.hints(&peer);
        let conn = tokio::time::timeout(CALL_TIMEOUT, net.dial(peer, protocol, versions, &hints, &hints))
            .await
            .map_err(|_| NodeError::Timeout("dialing the peer"))??;
        self.state().transports.insert(peer, conn.transport());
        Ok(conn)
    }

    /// One request on `omp/control/1`: hello both ways, then `request` and its answer.
    pub(crate) async fn control_call(&self, peer: NodeId, request: Kind) -> Result<Kind> {
        let conn = self.dial(peer, alpn::CONTROL).await?;
        let answer = tokio::time::timeout(CALL_TIMEOUT, control::call(self, &conn, request))
            .await
            .map_err(|_| NodeError::Timeout("the control request"));
        conn.close(0, b"done");
        answer?
    }

    /// Looks for nodes on the tailnet: every online tailnet machine is probed
    /// on this node's port; machines that answer the control handshake show
    /// up in [`Node::nearby`]. Returns how many answered.
    pub async fn scan_tailscale(&self) -> Result<usize> {
        let Some(executable) = self.inner.config.tailscale.clone() else {
            return Err(NodeError::Unavailable("tailscale is not installed".into()));
        };
        let port = match self.inner.config.bind.port() {
            0 => self.network()?.tcp().local_addr().port(),
            port => port,
        };
        let result = loa_net::tailscale::discover_from_status(&executable, Some(port), &control::Probe(self.clone())).await;
        let found = match result {
            Ok(found) => found,
            Err(error) => {
                self.state().tailscale = Availability::Failed(error.to_string());
                self.changed();
                return Err(error.into());
            }
        };
        let count = found.iter().filter(|p| p.node != self.inner.key).count();
        {
            let mut state = self.state();
            for peer in found.into_iter().filter(|p| p.node != self.inner.key) {
                let entry = state.nearby.entry(peer.node).or_insert_with(|| NearbyPeer {
                    key: peer.node,
                    name: None,
                    addresses: Vec::new(),
                    source: NearbySource::Tailscale,
                    seen_ms: 0,
                    app_version: None,
                    protocols: Vec::new(),
                });
                entry.source = NearbySource::Tailscale;
                entry.seen_ms = now_ms();
                if entry.name.is_none() {
                    entry.name = Some(peer.name.split('.').next().unwrap_or(&peer.name).to_owned());
                }
                if !entry.addresses.contains(&peer.address) {
                    entry.addresses.push(peer.address);
                }
            }
            state.tailscale = Availability::Active(format!("last scan found {count} node{}", if count == 1 { "" } else { "s" }));
        }
        let peers = self.peers_db().list().unwrap_or_default();
        for peer in peers {
            if let Some(nearby) = self.state().nearby.get(&peer.key).filter(|n| n.source == NearbySource::Tailscale).cloned() {
                let mut addresses = peer.addresses.clone();
                addresses.extend(nearby.addresses.iter().copied().filter(|a| !peer.addresses.contains(a)));
                let _ = self.peers_db().set_addresses(&peer.key, &addresses);
                self.state().nearby.remove(&peer.key);
            }
        }
        self.changed();
        Ok(count)
    }

    pub(crate) fn note_discovered(&self, found: loa_net::discovery::DiscoveredPeer) {
        if found.node == self.inner.key {
            return;
        }
        let known = self.peers_db().lookup(&found.node).ok().flatten();
        if let Some(peer) = known {
            if !found.addresses.is_empty() && peer.addresses != found.addresses {
                let _ = self.peers_db().set_addresses(&found.node, &found.addresses);
            }
        } else {
            let mut state = self.state();
            let entry = state.nearby.entry(found.node).or_insert_with(|| NearbyPeer {
                key: found.node,
                name: None,
                addresses: Vec::new(),
                source: NearbySource::Mdns,
                seen_ms: 0,
                app_version: None,
                protocols: Vec::new(),
            });
            entry.source = NearbySource::Mdns;
            entry.seen_ms = now_ms();
            entry.addresses = found.addresses;
            if found.name.is_some() {
                entry.name = found.name;
            }
        }
        self.changed();
    }

    /// Grants a peer `scopes` until `until_ms` (`None`: until revoked) and
    /// sends it the token. A holder that is offline gets it when it answers.
    pub async fn issue_grant(&self, holder: &str, scopes: Vec<Scope>, until_ms: Option<i64>, reshare: bool) -> Result<GrantId> {
        let holder = Self::parse_key(holder)?;
        let now = now_ms();
        let ttl_ms = until_ms.map_or(loa_grants::DEFAULT_TTL_MS, |until| (until - now).min(loa_grants::DEFAULT_TTL_MS));
        let spec = GrantSpec { scopes, ttl_ms, reshare, until_ms };
        let issued = self.inner.grants.issue(&self.inner.grant_key, grant_key(&holder), spec, now)?;
        self.changed();
        self.deliver(holder, &issued.id, issued.token).await;
        Ok(issued.id)
    }

    /// Moves the end of a grant this node issued and sends the holder a token.
    pub async fn renew_grant(&self, id: &GrantId, until_ms: Option<i64>) -> Result<()> {
        let issued = self.inner.grants.extend(&self.inner.grant_key, id, until_ms, now_ms())?;
        self.changed();
        if let Some(holder) = node_id(&issued.holder) {
            self.deliver(holder, id, issued.token).await;
        }
        Ok(())
    }

    /// Revokes a grant this node issued and tells its holder. Its streams stop
    /// at the next patch: every check looks the revocation up.
    pub async fn revoke_grant(&self, id: &GrantId) -> Result<()> {
        self.inner.grants.revoke(&self.inner.grant_key.public(), id, now_ms())?;
        self.state().undelivered.remove(id);
        self.changed();
        let holder = self.inner.grants.issued()?.into_iter().find(|g| &g.id == id).and_then(|g| node_id(&g.holder));
        if let Some(holder) = holder {
            let notice = Kind::GrantRevocation(wire::GrantRevocation { grant_id: id.as_str().as_bytes().to_vec() });
            if let Err(error) = self.control_call(holder, notice).await {
                tracing::debug!(%holder, %error, "revocation notice not delivered; the grant still ends at its next renewal");
            }
        }
        Ok(())
    }

    async fn deliver(&self, holder: NodeId, id: &GrantId, token: Vec<u8>) {
        let delivery = Kind::GrantDelivery(wire::GrantDelivery { token, root: Vec::new() });
        match self.control_call(holder, delivery).await {
            Ok(Kind::Ack(ack)) if ack.error.is_empty() => {
                self.state().undelivered.remove(id);
            }
            Ok(other) => {
                tracing::warn!(%holder, ?other, "grant delivery refused");
                self.state().undelivered.insert(id.clone());
            }
            Err(error) => {
                tracing::debug!(%holder, %error, "grant delivery deferred until the holder answers");
                self.state().undelivered.insert(id.clone());
            }
        }
    }

    async fn deliver_waiting(&self, peer: NodeId) {
        let waiting: Vec<GrantId> = self.state().undelivered.iter().cloned().collect();
        if waiting.is_empty() {
            return;
        }
        let holder = grant_key(&peer);
        let issued = self.inner.grants.issued().unwrap_or_default();
        for record in issued.into_iter().filter(|g| g.holder == holder && waiting.contains(&g.id) && g.revoked_ms.is_none()) {
            match self.inner.grants.extend(&self.inner.grant_key, &record.id, record.until_ms, now_ms()) {
                Ok(fresh) => self.deliver(peer, &record.id, fresh.token).await,
                Err(error) => {
                    tracing::debug!(id = %record.id, %error, "waiting grant can no longer be delivered");
                    self.state().undelivered.remove(&record.id);
                }
            }
        }
    }

    /// Renews held grants that expire within the renewal margin.
    pub async fn renew_due(&self) -> Vec<RenewOutcome> {
        let renewer = Renewer::new(self.inner.grant_key.public(), self.inner.config.renew_margin_ms);
        let outcomes = renewer
            .renew_due(&self.inner.grants, &control::RenewVia(self.clone()), now_ms())
            .await
            .unwrap_or_default();
        if !outcomes.is_empty() {
            self.changed();
        }
        outcomes
    }

    /// Grants this node issued, then grants it holds.
    pub fn grants(&self) -> Vec<GrantInfo> {
        let mut out = Vec::new();
        for record in self.inner.grants.issued().unwrap_or_default() {
            let Some(peer) = node_id(&record.holder) else { continue };
            out.push(GrantInfo {
                id: record.id,
                peer,
                direction: GrantDirection::Given,
                scopes: record.scopes.unwrap_or_default(),
                until_ms: record.until_ms,
                expires_ms: None,
                renews_at_ms: None,
                reshare: record.reshare,
                ended: record.revoked_ms.is_some(),
            });
        }
        for received in self.inner.grants.received_all().unwrap_or_default() {
            let Some(peer) = node_id(&received.issuer) else { continue };
            out.push(GrantInfo {
                scopes: received.scopes().unwrap_or_default(),
                id: received.id,
                peer,
                direction: GrantDirection::Received,
                until_ms: None,
                expires_ms: Some(received.expires_ms),
                renews_at_ms: received.refused_ms.is_none().then(|| received.expires_ms - self.inner.config.renew_margin_ms),
                reshare: received.reshare,
                ended: received.refused_ms.is_some(),
            });
        }
        out
    }

    /// Resources the app adds to grant pickers: SSH hosts, projects, surfaces.
    pub fn set_resources(&self, resources: Vec<(Resource, String)>) {
        self.state().resources = resources;
        self.changed();
    }

    /// What grants can name here, with labels: every future session, each
    /// project, session and prompt this node owns, and the app's extra
    /// resources. Future sessions carry `after_ms: 0`; issuing sets it to the
    /// issue time.
    pub fn resources(&self) -> Vec<(Resource, String)> {
        let mut out = vec![(Resource::FutureSessions { after_ms: 0 }, "Every future session".to_owned())];
        let sessions = self.inner.sessions.sessions();
        let mut projects: Vec<&str> = sessions.iter().filter_map(|session| session.facts.project.as_deref()).collect();
        projects.sort_unstable();
        projects.dedup();
        out.extend(projects.into_iter().map(|id| (Resource::Project { id: id.to_owned() }, format!("Project {id}"))));
        for session in &sessions {
            let label = session.label();
            out.push((Resource::Session { id: session.facts.id.clone() }, label.clone()));
            for (index, turn) in session.turns.iter().enumerate() {
                out.push((
                    Resource::Prompt { session: session.facts.id.clone(), turn: turn.id },
                    format!("{label} › prompt {}: {}", index + 1, turn.preview),
                ));
            }
        }
        let added = self.state().added.iter().map(|protocol| protocol.required.0.clone()).collect::<Vec<_>>();
        if added.contains(&Resource::Sandbox) {
            out.push((Resource::Sandbox, SANDBOX_LABEL.to_owned()));
        }
        // Folders already granted, to grant again; others are typed into the panel.
        let mut folders: Vec<String> = self
            .inner
            .grants
            .issued()
            .unwrap_or_default()
            .into_iter()
            .filter(|record| record.revoked_ms.is_none())
            .flat_map(|record| record.scopes.unwrap_or_default())
            .filter_map(|scope| match scope.resource {
                Resource::Folder { path } => Some(path),
                _ => None,
            })
            .collect();
        folders.sort_unstable();
        folders.dedup();
        out.extend(folders.into_iter().map(|path| (Resource::Folder { path: path.clone() }, format!("Folder {path}"))));
        out.extend(self.state().resources.iter().cloned());
        out
    }

    /// Serves `name`/`version` from the next [`Node::start`]: each stream
    /// reaches `handler` once its requester proved a grant for `required`.
    /// Adding a name and version again replaces the earlier handler.
    pub fn add_protocol(&self, name: &'static str, version: u32, required: (Resource, Action), handler: Arc<dyn PeerProtocol>) {
        let mut state = self.state();
        state.added.retain(|protocol| (protocol.name, protocol.version) != (name, version));
        state.added.push(AddedProtocol { name, version, required, handler });
    }

    /// The protocols added so far, to carry over to a node opened again.
    pub fn added_protocols(&self) -> Vec<AddedProtocol> {
        self.state().added.clone()
    }

    /// A stream on `name`/`version` to `peer`, after presenting the grants
    /// this node holds from it; refused when none covers the protocol.
    pub async fn open_stream(&self, peer: NodeId, name: &'static str, version: u32) -> Result<Stream> {
        extra::open(self, peer, name, version).await
    }

    /// Online peers whose live grants to this node cover `required`.
    pub fn granting_peers(&self, (resource, action): &(Resource, Action)) -> Vec<NodeId> {
        let now = now_ms();
        let granting: HashSet<NodeId> = self
            .grants()
            .into_iter()
            .filter(|grant| {
                grant.direction == GrantDirection::Received
                    && !grant.ended
                    && grant.expires_ms.is_none_or(|expires| expires > now)
                    && grant.scopes.iter().any(|scope| scope.resource == *resource && scope.actions.contains(action))
            })
            .map(|grant| grant.peer)
            .collect();
        self.peers().into_iter().filter(|peer| peer.online && granting.contains(&peer.key)).map(|peer| peer.key).collect()
    }

    /// Peers' folders this node's live grants reach, by peer and path.
    pub fn shared_folders(&self) -> Vec<SharedFolder> {
        let now = now_ms();
        let mut out: Vec<SharedFolder> = Vec::new();
        for grant in self.grants() {
            if grant.direction != GrantDirection::Received || grant.ended || grant.expires_ms.is_some_and(|expires| expires <= now) {
                continue;
            }
            for scope in grant.scopes {
                let Resource::Folder { path } = scope.resource else { continue };
                let can_write = scope.actions.contains(&Action::Write);
                match out.iter_mut().find(|folder| folder.peer == grant.peer && folder.path == path) {
                    Some(folder) => folder.can_write |= can_write,
                    None => out.push(SharedFolder { peer: grant.peer, path, can_write }),
                }
            }
        }
        out.sort_by(|a, b| a.peer.to_string().cmp(&b.peer.to_string()).then_with(|| a.path.cmp(&b.path)));
        out
    }

    /// The entries of `path` (relative, empty for the folder itself) in a
    /// peer's shared `folder`.
    pub async fn list_folder(&self, peer: &str, folder: &str, path: &str) -> Result<Vec<FolderEntry>> {
        folder::list_remote(self, Self::parse_key(peer)?, folder, path).await
    }

    /// The bytes of file `path` in a peer's shared `folder` (up to [`MAX_FOLDER_FILE`]).
    pub async fn read_folder_file(&self, peer: &str, folder: &str, path: &str) -> Result<Vec<u8>> {
        folder::read_remote(self, Self::parse_key(peer)?, folder, path).await
    }

    /// Writes file `path` in a peer's shared `folder` (needs a write grant).
    pub async fn write_folder_file(&self, peer: &str, folder: &str, path: &str, bytes: &[u8]) -> Result<()> {
        folder::write_remote(self, Self::parse_key(peer)?, folder, path, bytes).await
    }

    /// Sessions of `peer` this node's grants reach, asked over the network.
    pub async fn list_remote(&self, peer: &str) -> Result<Vec<RemoteSession>> {
        let peer = Self::parse_key(peer)?;
        let sessions = share::list(self, peer).await?;
        self.state().remote.insert(peer, sessions.clone());
        self.changed();
        Ok(sessions)
    }

    /// Every peer session last listed, for Recents (tagged by [`RemoteSession::peer`]).
    pub fn remote_sessions(&self) -> Vec<RemoteSession> {
        let state = self.state();
        let mut out: Vec<_> = state.remote.values().flatten().cloned().collect();
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Opens a live view of a peer's session. Drive requests go through it.
    pub async fn open_remote(&self, peer: &str, session: &str) -> Result<RemoteView> {
        let peer = Self::parse_key(peer)?;
        share::open(self, peer, session).await
    }

    /// Asks a peer's harness to run `text` in `session` (needs drive).
    pub async fn prompt_remote(&self, peer: &str, session: &str, text: &str) -> Result<()> {
        let peer = Self::parse_key(peer)?;
        share::prompt(self, peer, session, text).await
    }

    /// Copies a peer's session, as far as this node's grant shows it, into a
    /// new session this node owns (needs fork). Returns the new session id;
    /// the app resumes it on its own harness with its own credentials.
    pub async fn fork_remote(&self, peer: &str, session: &str) -> Result<String> {
        let Some(store) = self.inner.config.sessions.clone() else {
            return Err(NodeError::Unavailable("this node has no session store to fork into".into()));
        };
        let view = self.open_remote(peer, session).await?;
        let new_id = format!("{session}-fork-{}", now_ms());
        self.fork_view(&view, &store, &new_id)?;
        Ok(new_id)
    }

    /// Forks an open view into `store` as `new_id`.
    pub fn fork_view(&self, view: &RemoteView, store: &Store, new_id: &str) -> Result<Journal> {
        if !view.can(Action::Fork) {
            return Err(NodeError::Refused("the grant does not allow forking this session".into()));
        }
        let patches = view.patches();
        let at = patches.last().map_or(0, |p| p.seq);
        Ok(loa_share::fork_mirror_to_own_node(store, &patches, at, new_id, &self.inner.key.to_string())?)
    }
}

impl Inner {
    pub(crate) fn node(self: &Arc<Self>) -> Node {
        Node::from_inner(Arc::clone(self))
    }
}

async fn background(weak: Weak<Inner>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    loop {
        tick.tick().await;
        let Some(inner) = weak.upgrade() else { break };
        let node = inner.node();
        drop(inner);
        node.refresh_presence().await;
        node.renew_due().await;
    }
}

/// Resolves `host[:port]`; the port defaults to 31415.
async fn resolve(address: &str) -> Result<Vec<SocketAddr>> {
    if let Ok(addr) = address.parse::<SocketAddr>() {
        return Ok(vec![addr]);
    }
    let target = if address.rsplit_once(':').is_some_and(|(_, port)| port.parse::<u16>().is_ok()) {
        address.to_owned()
    } else {
        format!("{address}:{DEFAULT_PORT}")
    };
    let found: Vec<SocketAddr> = tokio::net::lookup_host(&target)
        .await
        .map_err(|e| NodeError::InvalidAddress(address.to_owned(), e.to_string()))?
        .collect();
    if found.is_empty() {
        return Err(NodeError::InvalidAddress(address.to_owned(), "no addresses".into()));
    }
    Ok(found)
}

/// Default directory of the node's stores inside the app data directory.
pub fn data_dir(app_data: &Path) -> PathBuf {
    app_data.join("node")
}
