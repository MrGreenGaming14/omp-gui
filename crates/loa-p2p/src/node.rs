//! [`Node`]: the facade the app drives. It owns the transports and composes
//! a [`PeerDirectory`], a [`GrantService`] and a [`SessionShare`].

use crate::auth::{GrantInfo, GrantService};
use crate::discovery::{Announce, Availability, Discoverer, Mdns, NearbyPeer, Tailnet};
use crate::peers_db::{DiscoverySource, PeerDirectory, PeerInfo};
use crate::protocols::session::{RemoteSession, RemoteView, SessionShare};
use crate::protocols::{Served, control, extra, folder, session};
use crate::protocols::extra::{AddedProtocol, PeerProtocol};
use crate::protocols::folder::{FolderEntry, SharedFolder};
use crate::sessions::LocalSessions;
use crate::transport::{Connection, FallbackDialer, Quic, RelayConfig, Router, Stream, Tcp, Transport, TransportKind};
use crate::wire::{self, Control, GrantDelivery, GrantRevocation, Hello, ProtocolVersion};
use crate::{DEFAULT_PORT, NodeError, NodeId, Result};
use loa_bus::{Journal, Store};
use loa_trust::{Action, GrantId, GrantSpec, GrantStore, KeySource, NodeIdentity, PublicKey, RenewOutcome, Renewer, Resource, Scope};
use loa_util::LockExt;
use loa_util::exec::which;
use loa_util::time::now_ms;
use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// How the panel names [`Resource::Sandbox`].
pub const SANDBOX_LABEL: &str = "Sandbox on this node";
/// How long one dial or control request may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long QUIC may take before TCP is tried.
const QUIC_TIMEOUT: Duration = Duration::from_millis(400);

/// The protocol labels this node announces in its hello.
pub fn local_protocols() -> Vec<String> { wire::LOCAL.iter().map(|l| String::from_utf8_lossy(l).into_owned()).collect() }

/// Default directory of the node's stores inside the app data directory.
pub fn data_dir(app_data: &Path) -> PathBuf { app_data.join("node") }

/// How the node runs. [`NodeConfig::new`] gives the app defaults.
#[derive(Clone, Debug)]
pub struct NodeConfig {
    /// Where `peers.sqlite` (and with [`Node::open`], `grants.sqlite`) live.
    pub data_dir: PathBuf,
    /// UDP and TCP listening address; port 0 picks free ports (tests).
    pub bind: SocketAddr,
    pub relay: RelayConfig,
    pub mdns: bool,
    /// The `tailscale` executable; `None` when not installed or turned off.
    pub tailscale: Option<PathBuf>,
    /// The name peers see.
    pub name: String,
    pub app_version: String,
    /// How often peers are probed and held grants renewed.
    pub presence_interval: Duration,
    /// Held grants renew this long before they expire.
    pub renew_margin_ms: i64,
    /// The session store forks of peers' sessions go into; `None` disables forking.
    pub sessions: Option<Store>,
}

impl NodeConfig {
    pub fn new(data_dir: impl Into<PathBuf>, name: impl Into<String>, app_version: impl Into<String>) -> Self {
        Self {
            data_dir: data_dir.into(),
            bind: SocketAddr::from((Ipv4Addr::UNSPECIFIED, DEFAULT_PORT)),
            relay: RelayConfig::Off,
            mdns: true,
            tailscale: which("tailscale"),
            name: name.into(),
            app_version: app_version.into(),
            presence_interval: Duration::from_secs(30),
            renew_margin_ms: 3 * 60_000,
            sessions: None,
        }
    }
}

/// State of the endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Stopped,
    Starting,
    Listening { udp: Vec<SocketAddr>, tcp: SocketAddr },
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

/// Something the app should react to.
#[derive(Clone, Debug)]
pub enum NodeEvent {
    /// Peers, grants, status or remote sessions changed.
    Changed,
    /// A peer with drive access asks this node's harness to run a prompt.
    PromptRequested { peer: NodeId, session: String, text: String },
    /// A peer with drive access answers decision `decision` (this node's id) of `session`.
    AnswerRequested { peer: NodeId, session: String, decision: loa_core::NodeId, values: Vec<String>, reason: Option<String> },
}

/// The running node. Cheap to clone; every clone is the same node.
#[derive(Clone)]
pub struct Node(pub(crate) Arc<Inner>);

pub(crate) struct Inner {
    config: NodeConfig,
    key: NodeId,
    pub(crate) peers: PeerDirectory,
    pub(crate) grants: GrantService,
    pub(crate) share: SessionShare,
    net: Mutex<Option<Arc<FallbackDialer>>>,
    state: Mutex<State>,
    events: broadcast::Sender<NodeEvent>,
}

struct State {
    status: NodeStatus,
    resources: Vec<(Resource, String)>,
    /// Protocols the app added; served from the next start.
    added: Vec<AddedProtocol>,
    tasks: Vec<JoinHandle<()>>,
}

fn create_dir(dir: &Path) -> Result<()> { std::fs::create_dir_all(dir).map_err(|e| NodeError::Unavailable(format!("{}: {e}", dir.display()))) }

impl Node {
    /// Opens the node's stores under `config.data_dir`, grants included. Nothing touches the network until [`Node::start`].
    pub fn open(config: NodeConfig, identity: &NodeIdentity, key_source: KeySource, sessions: Arc<dyn LocalSessions>) -> Result<Self> {
        create_dir(&config.data_dir)?;
        let grants = Arc::new(GrantStore::open(&config.data_dir.join("grants.sqlite"))?);
        Self::open_with_grants(config, identity, key_source, sessions, grants)
    }

    /// [`Node::open`] over the app's grant store, shared with whatever else checks grants.
    pub fn open_with_grants(config: NodeConfig, identity: &NodeIdentity, key_source: KeySource, sessions: Arc<dyn LocalSessions>, grants: Arc<GrantStore>) -> Result<Self> {
        create_dir(&config.data_dir)?;
        let key = NodeId::try_from(identity.public_key()).map_err(|e| NodeError::InvalidKey(e.to_string()))?;
        let tailscale = match &config.tailscale {
            Some(path) => Availability::Active(format!("{} found; scan to look for nodes", path.display())),
            None if which("tailscale").is_some() => Availability::Off("turned off".into()),
            None => Availability::Off("tailscale is not installed".into()),
        };
        let mdns = Availability::Off(if config.mdns { "starts with the endpoint" } else { "turned off" }.into());
        let status = NodeStatus { key, name: config.name.clone(), app_version: config.app_version.clone(), key_source, endpoint: Endpoint::Stopped, mdns, tailscale };
        Ok(Self(Arc::new(Inner {
            peers: PeerDirectory::open(&config.data_dir.join("peers.sqlite"))?,
            grants: GrantService::new(grants, identity.clone(), config.renew_margin_ms),
            share: SessionShare { local: sessions, remote: Mutex::default() },
            config,
            key,
            net: Mutex::new(None),
            state: Mutex::new(State { status, resources: Vec::new(), added: Vec::new(), tasks: Vec::new() }),
            events: broadcast::channel(256).0,
        })))
    }

    pub fn key(&self) -> NodeId { self.0.key }
    pub fn config(&self) -> &NodeConfig { &self.0.config }
    pub fn status(&self) -> NodeStatus { self.0.state.locked().status.clone() }
    /// The `Authorize` every session writer this node owns must use.
    pub fn authorizer(&self) -> Arc<dyn loa_bus::Authorize> { self.0.grants.access.clone() }
    pub fn grant_store(&self) -> &Arc<GrantStore> { &self.0.grants.store }
    /// Changes and requests, for the app's subscription.
    pub fn events(&self) -> broadcast::Receiver<NodeEvent> { self.0.events.subscribe() }
    pub(crate) fn emit(&self, event: NodeEvent) { let _ = self.0.events.send(event); }
    pub(crate) fn changed(&self) { self.emit(NodeEvent::Changed) }
    pub(crate) fn network(&self) -> Result<Arc<FallbackDialer>> { self.0.net.locked().clone().ok_or(NodeError::NotStarted) }

    fn status_with(&self, change: impl FnOnce(&mut NodeStatus)) {
        change(&mut self.0.state.locked().status);
        self.changed();
    }

    /// Binds both transports and starts discovery, presence and renewal; a running node does nothing.
    pub async fn start(&self) -> Result<()> {
        if self.0.net.locked().is_some() {
            return Ok(());
        }
        self.status_with(|s| s.endpoint = Endpoint::Starting);
        let weak = Arc::downgrade(&self.0);
        let mut router = Router::default()
            .handler(wire::CONTROL, Arc::new(Served(weak.clone(), control::serve)))
            .handler(wire::SESSION, Arc::new(Served(weak.clone(), session::serve)))
            .handler(wire::FOLDER, Arc::new(Served(weak.clone(), folder::serve)));
        for added in self.added_protocols() {
            let label = added.label();
            router = router.handler(label, Arc::new(Served(weak.clone(), move |node, conn, stream| extra::serve(node, conn, stream, added.clone()))));
        }
        let (config, seed) = (&self.0.config, *self.0.grants.identity.secret_bytes());
        let bound = async {
            let quic = Quic::bind(seed, config.bind, config.relay.clone(), router.clone()).await?;
            Ok::<_, NodeError>((quic, Tcp::bind(seed, config.bind, router).await?))
        };
        let (quic, tcp) = match bound.await {
            Ok(bound) => bound,
            Err(error) => {
                self.status_with(|s| s.endpoint = Endpoint::Failed(error.to_string()));
                return Err(error);
            }
        };
        let (udp, tcp_addr) = (quic.local_addrs(), tcp.local_addrs()[0]);
        *self.0.net.locked() = Some(Arc::new(FallbackDialer { transports: vec![Arc::new(quic), Arc::new(tcp)], first_timeout: QUIC_TIMEOUT }));
        let mut tasks = vec![tokio::spawn(background(weak.clone(), config.presence_interval))];
        let mdns = if config.mdns {
            let announce = Announce { node: self.0.key, udp_port: udp.first().map_or(0, SocketAddr::port), tcp_port: tcp_addr.port(), name: config.name.clone() };
            match Mdns::start(&announce) {
                Ok(mdns) => {
                    let availability = mdns.availability();
                    tasks.push(tokio::spawn(discover(weak, mdns)));
                    availability
                }
                Err(error) => Availability::Failed(error.to_string()),
            }
        } else {
            Availability::Off("turned off".into())
        };
        self.0.state.locked().tasks = tasks;
        self.status_with(|s| (s.endpoint, s.mdns) = (Endpoint::Listening { udp, tcp: tcp_addr }, mdns));
        Ok(())
    }

    /// Stops discovery and closes the endpoint.
    pub async fn stop(&self) {
        std::mem::take(&mut self.0.state.locked().tasks).iter().for_each(JoinHandle::abort);
        let net = self.0.net.locked().take();
        if let Some(net) = net {
            net.close().await;
        }
        self.0.peers.clear_transports();
        self.status_with(|s| {
            s.endpoint = Endpoint::Stopped;
            if matches!(s.mdns, Availability::Active(_)) {
                s.mdns = Availability::Off("stopped with the endpoint".into());
            }
        });
    }

    /// Parses a node key: 64 hex or 52 base32 characters, or the Biscuit form `ed25519/<hex>`.
    pub fn parse_key(text: &str) -> Result<NodeId> {
        let text = text.trim();
        let key = PublicKey::from_str(text.strip_prefix("ed25519/").unwrap_or(text)).ok().and_then(|key| NodeId::try_from(key).ok());
        key.ok_or_else(|| NodeError::InvalidKey(text.to_owned()))
    }

    /// Saves a peer by key, with an optional `host[:port]` to reach it, then probes it.
    pub async fn add_peer(&self, key: &str, address: Option<&str>) -> Result<()> {
        let key = Self::parse_key(key)?;
        if key == self.0.key {
            return Err(NodeError::InvalidKey("this node's own key".into()));
        }
        let addresses = match address.map(str::trim).filter(|a| !a.is_empty()) {
            Some(address) => resolve(address).await?,
            None => self.0.peers.hints(&key),
        };
        let name = self.0.peers.nearby().into_iter().find(|n| n.key == key).and_then(|n| n.name);
        self.0.peers.save(&key, name.as_deref(), &addresses, DiscoverySource::Manual)?;
        self.changed();
        self.probe(key).await;
        Ok(())
    }

    /// Saves a node found on the LAN, the tailnet or that connected here.
    pub async fn add_nearby(&self, key: &str) -> Result<()> {
        let key = Self::parse_key(key)?;
        self.0.peers.save_nearby(&key)?;
        self.changed();
        self.probe(key).await;
        Ok(())
    }

    pub fn rename_peer(&self, key: &str, name: &str) -> Result<()> { self.known(key, |node, key| node.0.peers.rename(key, name.trim())) }

    /// Forgets a peer. Grants given to it stay until revoked.
    pub fn remove_peer(&self, key: &str) -> Result<()> {
        self.known(key, |node, key| {
            node.0.share.remote.locked().remove(key);
            node.0.peers.remove(key)
        })
    }

    fn known(&self, key: &str, change: impl FnOnce(&Self, &NodeId) -> bool) -> Result<()> {
        let key = Self::parse_key(key)?;
        if !change(self, &key) {
            return Err(NodeError::UnknownPeer(key.to_string()));
        }
        self.changed();
        Ok(())
    }

    pub fn peers(&self) -> Vec<PeerInfo> { self.0.peers.list() }
    pub fn nearby(&self) -> Vec<NearbyPeer> { self.0.peers.nearby() }
    /// A peer's display name: the user's, its own, or the start of its key.
    pub fn peer_name(&self, key: &NodeId) -> String { self.0.peers.name(key) }

    /// Probes every saved peer, refreshing what each shares and delivering waiting grants.
    pub async fn refresh_presence(&self) {
        let mut probes = tokio::task::JoinSet::new();
        for peer in self.0.peers.keys() {
            let node = self.clone();
            probes.spawn(async move { node.probe(peer).await });
        }
        while probes.join_next().await.is_some() {}
    }

    async fn probe(&self, peer: NodeId) {
        if let Err(error) = self.control_call(peer, control::presence()).await {
            tracing::debug!(%peer, %error, "peer unreachable");
            self.0.peers.offline(&peer);
            return self.changed();
        }
        self.deliver_waiting(peer).await;
        if self.0.grants.holds_from(&peer) {
            let _ = self.list_remote(&peer.to_string()).await;
        } else if self.0.share.remote.locked().remove(&peer).is_some() {
            // Its grants ended: its sessions leave Recents.
            self.changed();
        }
    }

    /// Records what a connection's hello showed about `peer`.
    pub(crate) fn observe(&self, peer: NodeId, hello: &Hello, kind: TransportKind) {
        self.0.peers.observe(peer, hello, kind);
        self.changed();
    }

    pub(crate) fn hello(&self) -> Hello {
        let protocols = local_protocols().into_iter().filter_map(|label| {
            let (name, version) = label.rsplit_once('/')?;
            Some(ProtocolVersion { name: name.to_owned(), version: version.parse().ok()? })
        });
        Hello { app_version: self.0.config.app_version.clone(), protocols: protocols.collect(), name: self.0.config.name.clone() }
    }

    /// Dials `peer` on the exact `label`: QUIC first, then TCP, on its address hints.
    pub(crate) async fn dial(&self, peer: NodeId, label: &[u8]) -> Result<Connection> {
        let (net, hints) = (self.network()?, self.0.peers.hints(&peer));
        tokio::time::timeout(CALL_TIMEOUT, net.dial(peer, &hints, label)).await.map_err(|_| NodeError::Timeout("dialing the peer"))?
    }

    /// One request on `omp/control/1`: hellos both ways, then `request` and its answer.
    pub(crate) async fn control_call(&self, peer: NodeId, request: Control) -> Result<Control> {
        let conn = self.dial(peer, wire::CONTROL).await?;
        let answer = tokio::time::timeout(CALL_TIMEOUT, control::call(self, &conn, request)).await;
        conn.close(0, b"done");
        answer.map_err(|_| NodeError::Timeout("the control request"))?
    }

    /// Probes every online tailnet machine on this node's port; those that
    /// answer show up in [`Node::nearby`]. Returns how many answered.
    pub async fn scan_tailscale(&self) -> Result<usize> {
        let executable = self.0.config.tailscale.clone().ok_or_else(|| NodeError::Unavailable("tailscale is not installed".into()))?;
        let port = match (self.0.config.bind.port(), self.status().endpoint) {
            (0, Endpoint::Listening { tcp, .. }) => tcp.port(),
            (0, _) => return Err(NodeError::NotStarted),
            (port, _) => port,
        };
        let mut tailnet = Tailnet::new(executable, port, control::Probe(self.clone()));
        let mut count = 0;
        while let Some(found) = tailnet.next().await {
            if found.key != self.0.key {
                self.0.peers.found(found);
                count += 1;
            }
        }
        let availability = match tailnet.availability() {
            Availability::Failed(error) => Err(error),
            _ => Ok(Availability::Active(format!("last scan found {count} node{}", if count == 1 { "" } else { "s" }))),
        };
        self.status_with(|s| s.tailscale = availability.clone().unwrap_or_else(Availability::Failed));
        availability.map(|_| count).map_err(NodeError::Unavailable)
    }

    /// Grants a peer `scopes` until `until_ms` (`None`: until revoked) and
    /// sends it the token; an offline holder gets it when it answers.
    pub async fn issue_grant(&self, holder: &str, scopes: Vec<Scope>, until_ms: Option<i64>, reshare: bool) -> Result<GrantId> {
        let (holder, now) = (Self::parse_key(holder)?, now_ms());
        let ttl_ms = until_ms.map_or(loa_trust::DEFAULT_TTL_MS, |until| (until - now).min(loa_trust::DEFAULT_TTL_MS));
        let grants = &self.0.grants;
        let issued = grants.store.issue(&grants.identity, PublicKey::from(holder), GrantSpec { scopes, ttl_ms, reshare, until_ms }, now)?;
        self.changed();
        self.deliver(holder, &issued.id, issued.token).await;
        Ok(issued.id)
    }

    /// Moves the end of a grant this node issued and sends the holder a fresh token.
    pub async fn renew_grant(&self, id: &GrantId, until_ms: Option<i64>) -> Result<()> {
        let issued = self.0.grants.store.extend(&self.0.grants.identity, id, until_ms, now_ms())?;
        self.changed();
        if let Ok(holder) = NodeId::try_from(issued.holder) {
            self.deliver(holder, id, issued.token).await;
        }
        Ok(())
    }

    /// Revokes a grant this node issued and tells its holder; its streams stop at the next patch.
    pub async fn revoke_grant(&self, id: &GrantId) -> Result<()> {
        let grants = &self.0.grants;
        grants.store.revoke(&grants.key(), id, now_ms())?;
        grants.undelivered.locked().remove(id);
        self.changed();
        let holder = grants.store.issued()?.into_iter().find(|g| &g.id == id).and_then(|g| NodeId::try_from(g.holder).ok());
        if let Some(holder) = holder {
            let notice = Control::GrantRevocation(GrantRevocation { grant_id: id.as_str().as_bytes().to_vec() });
            if let Err(error) = self.control_call(holder, notice).await {
                tracing::debug!(%holder, %error, "revocation notice not delivered; the grant ends at its next renewal");
            }
        }
        Ok(())
    }

    async fn deliver(&self, holder: NodeId, id: &GrantId, token: Vec<u8>) {
        let delivered = self.control_call(holder, Control::GrantDelivery(GrantDelivery { token, root: Vec::new() })).await;
        let mut waiting = self.0.grants.undelivered.locked();
        match delivered {
            Ok(_) => waiting.remove(id),
            Err(error) => {
                tracing::debug!(%holder, %error, "grant delivery deferred until the holder answers");
                waiting.insert(id.clone())
            }
        };
    }

    async fn deliver_waiting(&self, peer: NodeId) {
        let grants = &self.0.grants;
        let waiting: HashSet<GrantId> = grants.undelivered.locked().clone();
        let holder = PublicKey::from(peer);
        for record in grants.store.issued().unwrap_or_default().into_iter().filter(|g| g.holder == holder && waiting.contains(&g.id) && g.revoked_ms.is_none()) {
            match grants.store.extend(&grants.identity, &record.id, record.until_ms, now_ms()) {
                Ok(fresh) => self.deliver(peer, &record.id, fresh.token).await,
                Err(error) => {
                    tracing::debug!(id = %record.id, %error, "waiting grant can no longer be delivered");
                    grants.undelivered.locked().remove(&record.id);
                }
            }
        }
    }

    /// Renews held grants that expire within the renewal margin.
    pub async fn renew_due(&self) -> Vec<RenewOutcome> {
        let renewer = Renewer::new(self.0.grants.key(), self.0.config.renew_margin_ms);
        let outcomes = renewer.renew_due(&self.0.grants.store, &control::RenewVia(self.clone()), now_ms()).await.unwrap_or_default();
        if !outcomes.is_empty() {
            self.changed();
        }
        outcomes
    }

    /// Grants this node issued, then grants it holds.
    pub fn grants(&self) -> Vec<GrantInfo> { self.0.grants.infos() }

    /// Resources the app adds to grant pickers: SSH hosts, surfaces.
    pub fn set_resources(&self, resources: Vec<(Resource, String)>) {
        self.0.state.locked().resources = resources;
        self.changed();
    }

    /// What grants can name here, labelled: future sessions (`after_ms: 0`,
    /// set when issued), projects, sessions and prompts, then extra resources.
    pub fn resources(&self) -> Vec<(Resource, String)> {
        let sessions = self.0.share.local.sessions();
        let mut projects: Vec<&str> = sessions.iter().filter_map(|session| session.facts.project.as_deref()).collect();
        projects.sort_unstable();
        projects.dedup();
        let mut out = vec![(Resource::FutureSessions { after_ms: 0 }, "Every future session".to_owned())];
        out.extend(projects.into_iter().map(|id| (Resource::Project { id: id.to_owned() }, format!("Project {id}"))));
        for session in &sessions {
            let label = session.label();
            out.push((Resource::Session { id: session.facts.id.clone() }, label.clone()));
            out.extend(session.turns.iter().enumerate().map(|(i, turn)| {
                (Resource::Prompt { session: session.facts.id.clone(), turn: turn.id }, format!("{label} › prompt {}: {}", i + 1, turn.preview))
            }));
        }
        let state = self.0.state.locked();
        if state.added.iter().any(|protocol| protocol.required.0 == Resource::Sandbox) {
            out.push((Resource::Sandbox, SANDBOX_LABEL.to_owned()));
        }
        // Folders already granted, to grant again; others are typed into the panel.
        let issued = self.0.grants.store.issued().unwrap_or_default().into_iter().filter(|record| record.revoked_ms.is_none());
        let mut folders: Vec<String> = issued.flat_map(|record| record.scopes.unwrap_or_default()).filter_map(|scope| match scope.resource {
            Resource::Folder { path } => Some(path),
            _ => None,
        }).collect();
        folders.sort_unstable();
        folders.dedup();
        out.extend(folders.into_iter().map(|path| (Resource::Folder { path: path.clone() }, format!("Folder {path}"))));
        out.extend(state.resources.iter().cloned());
        out
    }

    /// Serves `name/version` from the next [`Node::start`] to requesters proving a grant for `required`.
    pub fn add_protocol(&self, name: &'static str, version: u32, required: (Resource, Action), handler: Arc<dyn PeerProtocol>) {
        self.add(AddedProtocol { name, version, required, handler });
    }

    pub(crate) fn add(&self, protocol: AddedProtocol) {
        let added = &mut self.0.state.locked().added;
        added.retain(|known| known.label() != protocol.label());
        added.push(protocol);
    }

    /// The protocols added so far.
    pub fn added_protocols(&self) -> Vec<AddedProtocol> { self.0.state.locked().added.clone() }

    /// A stream on `name/version` to `peer` past the grant handshake; refused without a covering grant.
    pub async fn open_stream(&self, peer: NodeId, name: &'static str, version: u32) -> Result<Stream> { extra::open(self, peer, name, version).await }

    /// Live received grants, each with the peer that gave it.
    fn live_received(&self) -> impl Iterator<Item = GrantInfo> {
        let now = now_ms();
        self.grants().into_iter().filter(move |grant| grant.live_received(now))
    }

    /// Online peers whose live grants to this node cover `required`.
    pub fn granting_peers(&self, (resource, action): &(Resource, Action)) -> Vec<NodeId> {
        let granting: HashSet<NodeId> = self.live_received()
            .filter(|grant| grant.scopes.iter().any(|scope| scope.resource == *resource && scope.actions.contains(action)))
            .map(|grant| grant.peer)
            .collect();
        self.peers().into_iter().filter(|peer| peer.online && granting.contains(&peer.key)).map(|peer| peer.key).collect()
    }

    /// Peers' folders this node's live grants reach, by peer and path.
    pub fn shared_folders(&self) -> Vec<SharedFolder> {
        let mut out: Vec<SharedFolder> = Vec::new();
        for (peer, scope) in self.live_received().flat_map(|grant| grant.scopes.into_iter().map(move |scope| (grant.peer, scope))) {
            let Resource::Folder { path } = scope.resource else { continue };
            let can_write = scope.actions.contains(&Action::Write);
            match out.iter_mut().find(|folder| folder.peer == peer && folder.path == path) {
                Some(folder) => folder.can_write |= can_write,
                None => out.push(SharedFolder { peer, path, can_write }),
            }
        }
        out.sort_by(|a, b| a.peer.to_string().cmp(&b.peer.to_string()).then_with(|| a.path.cmp(&b.path)));
        out
    }

    /// The entries of `path` (relative; empty for the folder) in a peer's shared `folder`.
    pub async fn list_folder(&self, peer: &str, folder: &str, path: &str) -> Result<Vec<FolderEntry>> { folder::list_remote(self, Self::parse_key(peer)?, folder, path).await }

    /// A file of a peer's shared folder, up to [`crate::MAX_FOLDER_FILE`].
    pub async fn read_folder_file(&self, peer: &str, folder: &str, path: &str) -> Result<Vec<u8>> { folder::read_remote(self, Self::parse_key(peer)?, folder, path).await }

    /// Writes a file in a peer's shared folder (needs a write grant).
    pub async fn write_folder_file(&self, peer: &str, folder: &str, path: &str, bytes: &[u8]) -> Result<()> {
        folder::write_remote(self, Self::parse_key(peer)?, folder, path, bytes).await
    }

    /// Sessions of `peer` this node's grants reach, asked over the network.
    pub async fn list_remote(&self, peer: &str) -> Result<Vec<RemoteSession>> {
        let peer = Self::parse_key(peer)?;
        let sessions = session::list(self, peer).await?;
        self.0.share.remote.locked().insert(peer, sessions.clone());
        self.changed();
        Ok(sessions)
    }

    /// Every peer session last listed, newest first, for Recents.
    pub fn remote_sessions(&self) -> Vec<RemoteSession> {
        let mut out: Vec<_> = self.0.share.remote.locked().values().flatten().cloned().collect();
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Opens a live view of a peer's session; drive requests go through it.
    pub async fn open_remote(&self, peer: &str, session: &str) -> Result<RemoteView> { session::open(self, Self::parse_key(peer)?, session).await }

    /// Asks a peer's harness to run `text` in `session` (needs drive).
    pub async fn prompt_remote(&self, peer: &str, session: &str, text: &str) -> Result<()> { session::prompt(self, Self::parse_key(peer)?, session, text).await }

    /// Copies a peer's session, as far as the grant shows it, into a new
    /// session this node owns (needs fork); returns its id.
    pub async fn fork_remote(&self, peer: &str, session: &str) -> Result<String> {
        let store = self.0.config.sessions.clone().ok_or_else(|| NodeError::Unavailable("this node has no session store to fork into".into()))?;
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
        crate::protocols::projection::fork_mirror_to_own_node(store, &patches, at, new_id, &self.0.key.to_string())
    }
}

async fn background(weak: Weak<Inner>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    loop {
        tick.tick().await;
        let Some(node) = weak.upgrade().map(Node) else { break };
        node.refresh_presence().await;
        node.renew_due().await;
    }
}

/// Feeds a discoverer's findings into the directory while the node lives.
async fn discover(weak: Weak<Inner>, mut discoverer: impl Discoverer) {
    while let Some(found) = discoverer.next().await {
        let Some(node) = weak.upgrade().map(Node) else { break };
        node.0.peers.found(found);
        node.changed();
    }
}

/// Resolves `host[:port]`; the port defaults to [`DEFAULT_PORT`].
async fn resolve(address: &str) -> Result<Vec<SocketAddr>> {
    if let Ok(addr) = address.parse::<SocketAddr>() {
        return Ok(vec![addr]);
    }
    let has_port = address.rsplit_once(':').is_some_and(|(_, port)| port.parse::<u16>().is_ok());
    let target = if has_port { address.to_owned() } else { format!("{address}:{DEFAULT_PORT}") };
    let invalid = |reason: String| NodeError::InvalidAddress(address.to_owned(), reason);
    let found: Vec<SocketAddr> = tokio::net::lookup_host(&target).await.map_err(|e| invalid(e.to_string()))?.collect();
    if found.is_empty() { Err(invalid("no addresses".into())) } else { Ok(found) }
}

