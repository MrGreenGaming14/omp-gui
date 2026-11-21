//! This node on the peer network: its key, the iroh endpoint (UDP) with the
//! TCP fallback on `[network] port`, LAN and tailnet discovery, the Peers
//! panel tab (peers, grants given and received, peers' sessions and shared
//! folders), peers' sessions in Recents tagged by node, a peer's session
//! opened in the center to watch, prompt, answer or fork, and a peer's shared
//! folder browsed in the center, its files opened and (with drive) saved.
//!
//! The node checks every peer request on this node's sessions against the
//! grants in `Services::grants`; peers' prompts and decision answers run
//! through this app's harness like the user's own.

mod adapter;
mod folder;
mod remote;
#[cfg(test)]
mod tests;

use std::hash::{Hash, Hasher};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use iced::futures::{SinkExt, Stream};
use iced::{Element, Subscription, Task};
use loa_contracts::harness::{Answer, Prompt};
use loa_config::{NetworkSettings, Relay};
use loa_trust::Resource;
use adapter::{self as ui, Outcome};
use loa_p2p::{Endpoint, LibrarySessions, Node, NodeConfig, NodeEvent, RelayConfig};
use loa_ui_shell::palette;
use loa_ui_peers as peers;
use loa_ui_shell::{NodeStatus, SessionSummary};

use crate::area::{Area, AreaId, Ctx, PanelTab, Request};

pub use folder::FolderMessage;
pub use remote::{Feed, RemoteMessage};

/// Id of the Peers panel tab.
const TAB: &str = "peers";

/// Recents ids of peers' sessions: `peer:<node key>:<session id>`.
const RECENT_PREFIX: &str = "peer:";

const COPY_KEY: &str = "copy_key";
const REFRESH: &str = "refresh";
const SCAN: &str = "scan_tailnet";
const RESTART: &str = "restart";

#[derive(Clone, Debug)]
pub enum Message {
    Panel(peers::Message),
    /// Show the Peers tab (the sidebar's node row).
    Open,
    /// Re-read the node and grants without switching the panel tab, for
    /// Settings › Peers & sharing.
    Refresh,
    /// The endpoint started, or why it didn't.
    Started(Result<(), String>),
    /// The node changed or a peer asks something of this node.
    Node(NodeEvent),
    /// A panel action finished.
    Applied(Result<Outcome, String>),
    /// SSH hosts and surfaces grants can name.
    Resources(Vec<(Resource, String)>),
    /// The old endpoint stopped; open the node again with the saved settings.
    Stopped,
    /// A peer's session picked in Recents.
    OpenRecent(String),
    /// Delete on a peer's session in Recents.
    ForgetRecent(String),
    Remote(RemoteMessage),
    Folder(FolderMessage),
}

pub struct State {
    panel: peers::State,
    /// `None` when its stores couldn't be opened; the reason was shown.
    node: Option<Node>,
    /// Bumped each time the node is opened again, so its event feed restarts.
    generation: u64,
    /// A peer's session shown in the center.
    remote: Option<remote::Remote>,
    /// A peer's shared folder shown in the center.
    folder: Option<folder::Browser>,
    /// Recents ids of the peers' sessions listed in the sidebar.
    listed: Vec<String>,
}

/// Whether a Recents id names a peer's session.
pub fn is_remote(id: &str) -> bool {
    id.starts_with(RECENT_PREFIX)
}

fn recent_id(peer: &str, session: &str) -> String {
    format!("{RECENT_PREFIX}{peer}:{session}")
}

/// The peer key and session id of a Recents id.
fn parse_recent(id: &str) -> Option<(&str, &str)> {
    id.strip_prefix(RECENT_PREFIX)?.split_once(':')
}

/// The node's settings from `[network]`: the port for UDP and TCP, the
/// relay, and which discovery runs.
pub(crate) fn node_config(ctx: &Ctx, network: &NetworkSettings) -> NodeConfig {
    let services = &ctx.services;
    let mut config = NodeConfig::new(loa_p2p::data_dir(&services.dirs.data), services.node.label.clone(), env!("CARGO_PKG_VERSION"));
    config.bind = SocketAddr::from((Ipv4Addr::UNSPECIFIED, network.port));
    // loa-p2p reaches peers through relays the user runs; it has no public ones.
    config.relay = match &network.relay {
        Relay::Url(url) => RelayConfig::SelfHosted(url.clone()),
        // TODO(relay): `Relay::Default` maps to Off (no public relays); kept as is pending a product decision (#292).
        Relay::Default | Relay::Off => RelayConfig::Off,
    };
    config.mdns = network.discovery.mdns;
    if !network.discovery.tailscale {
        config.tailscale = None;
    }
    config.sessions = Some(services.library.store().clone());
    config
}

/// `[network]` from the central config; the defaults when it can't be read.
fn network_settings(ctx: &mut Ctx) -> NetworkSettings {
    ctx.read_config("network settings, using the defaults").unwrap_or_default().network
}

impl State {
    pub fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let network = network_settings(ctx);
        let config = node_config(ctx, &network);
        Self::with_config(ctx, config)
    }

    /// Opens the node over the app's sessions and grant store with
    /// `config`; the task starts its endpoint.
    pub(crate) fn with_config(ctx: &mut Ctx, config: NodeConfig) -> (Self, Task<Message>) {
        let mut state =
            Self { panel: peers::State::default(), node: None, generation: 0, remote: None, folder: None, listed: Vec::new() };
        let task = state.open(ctx, config);
        (state, task)
    }

    fn open(&mut self, ctx: &mut Ctx, config: NodeConfig) -> Task<Message> {
        self.generation += 1;
        let services = &ctx.services;
        let sessions = Arc::new(LibrarySessions::new(Arc::clone(&services.library)));
        let opened = Node::open_with_grants(
            config,
            &services.node.identity,
            services.node.key_source.clone(),
            sessions,
            Arc::clone(&services.grants),
        );
        match opened {
            Ok(node) => {
                services.peer_access.set(node.authorizer());
                ctx.node.set(Some(node.clone()));
                let start = node.clone();
                self.node = Some(node);
                self.refresh(ctx);
                Task::batch([
                    Task::perform(async move { start.start().await.map_err(|error| error.to_string()) }, Message::Started),
                    self.resources(ctx),
                ])
            }
            Err(error) => {
                self.node = None;
                ctx.node.set(None);
                ctx.notify(format!("The peer node can't open its stores: {error}"));
                self.refresh(ctx);
                Task::none()
            }
        }
    }

    /// The running node, if it opened.
    #[cfg(test)]
    pub(crate) fn node(&self) -> Option<&Node> {
        self.node.as_ref()
    }

    /// The peer's session shown in the center.
    #[cfg(test)]
    pub(crate) fn remote(&self) -> Option<&remote::Remote> {
        self.remote.as_ref()
    }

    /// The peer's shared folder shown in the center.
    #[cfg(test)]
    pub(crate) fn folder(&self) -> Option<&folder::Browser> {
        self.folder.as_ref()
    }

    /// Brings the panel, the sidebar's nodes and peers' sessions, and
    /// `ctx.peers` in line with the node.
    fn refresh(&mut self, ctx: &mut Ctx) {
        let local = &ctx.services.node;
        let Some(node) = &self.node else {
            ctx.shell.sidebar_mut().set_nodes(vec![NodeStatus { key: local.key.clone(), name: local.label.clone(), online: false, local: true }]);
            ctx.peers.clear();
            return;
        };
        self.panel.set_data(ui::data(node, loa_core::now_ms()));
        let online = matches!(node.status().endpoint, Endpoint::Listening { .. });
        let peers = node.peers();
        let mut nodes = vec![NodeStatus { key: local.key.clone(), name: local.label.clone(), online, local: true }];
        nodes.extend(peers.iter().map(|peer| NodeStatus {
            key: peer.key.to_string(),
            name: node.peer_name(&peer.key),
            online: peer.online,
            local: false,
        }));
        ctx.shell.sidebar_mut().set_nodes(nodes);
        ctx.peers = peers;
        let sessions = node.remote_sessions();
        let ids: Vec<String> = sessions.iter().map(|session| recent_id(&session.peer.to_string(), &session.id)).collect();
        for stale in self.listed.iter().filter(|id| !ids.contains(id)) {
            ctx.shell.sidebar_mut().remove_session(stale);
        }
        for (session, id) in sessions.into_iter().zip(&ids) {
            ctx.shell.sidebar_mut().upsert_session(SessionSummary {
                id: id.clone(),
                title: if session.title.is_empty() { format!("Session {}", session.id) } else { session.title },
                project: None,
                node: session.peer.to_string(),
                pinned: false,
                waiting: false,
                updated: session.created_ms,
            });
        }
        self.listed = ids;
    }

    /// Looks up the SSH hosts (`[remote_hosts]`) and the surfaces of the
    /// running MCP servers that grants can name.
    fn resources(&self, ctx: &mut Ctx) -> Task<Message> {
        if self.node.is_none() {
            return Task::none();
        }
        let hosts: Vec<String> = ctx.read_config("SSH hosts").unwrap_or_default().remote_hosts.into_keys().collect();
        let mcp = ctx.services.mcp.clone();
        Task::perform(
            async move {
                let mut resources: Vec<(Resource, String)> =
                    hosts.into_iter().map(|host| (Resource::Ssh { host: host.clone() }, format!("SSH {host}"))).collect();
                for spec in mcp.specs() {
                    // Servers that aren't running or declare no surfaces offer none.
                    for (uri, _) in mcp.surfaces(&spec.name).await.unwrap_or_default() {
                        let uri = uri.to_string();
                        resources.push((Resource::Surface { uri: uri.clone() }, format!("Surface {uri}")));
                    }
                }
                resources
            },
            Message::Resources,
        )
    }

    /// Runs a panel event on the node.
    fn apply(&self, event: peers::Event, ctx: &mut Ctx) -> Task<Message> {
        let Some(node) = self.node.clone() else {
            ctx.notify("The peer node isn't running; see the notice from startup");
            return Task::none();
        };
        Task::perform(ui::apply(node, event), Message::Applied)
    }

    fn applied(&mut self, outcome: Outcome, ctx: &mut Ctx) -> Task<Message> {
        let task = match outcome {
            Outcome::Done => Task::none(),
            Outcome::Notice(notice) => {
                ctx.notify(notice);
                Task::none()
            }
            Outcome::KeyChecked { key, valid } => {
                self.panel.set_key_validation(&key, valid);
                Task::none()
            }
            Outcome::Copy(text) => {
                ctx.notify("Key copied");
                iced::clipboard::write(text)
            }
            Outcome::Opened(view) => {
                let name = self.node.as_ref().map_or_else(|| view.peer().fmt_short().to_string(), |node| node.peer_name(&view.peer()));
                let theme = ctx.shell.theme();
                self.remote = Some(remote::Remote::new(view, name, ctx.services.node.key.clone(), &theme));
                self.folder = None;
                ctx.show_screen(AreaId::Peers);
                Task::none()
            }
            Outcome::Forked(id) => {
                ctx.request(Request::OpenSession(id));
                ctx.notify("Forked into a session on this node; it runs on this node's harness and keys");
                Task::none()
            }
            Outcome::Folder { peer, folder, entries, can_write } => {
                if let Some(node) = &self.node {
                    let name = Node::parse_key(&peer).map_or_else(|_| peer.clone(), |key| node.peer_name(&key));
                    self.folder = Some(folder::Browser::new(node.clone(), peer, name, folder, entries, can_write));
                    self.remote = None;
                    ctx.show_screen(AreaId::Peers);
                }
                Task::none()
            }
        };
        self.refresh(ctx);
        task
    }

    /// A peer asks this node to run a prompt or answer a decision.
    fn node_event(&mut self, event: NodeEvent, ctx: &mut Ctx) {
        match event {
            NodeEvent::Changed => self.refresh(ctx),
            NodeEvent::PromptRequested { peer, session, text } => {
                let name = self.node.as_ref().map_or_else(|| peer.fmt_short().to_string(), |node| node.peer_name(&peer));
                ctx.request(Request::OpenSession(session));
                ctx.request(Request::Prompt(Prompt { text, attachments: Vec::new(), by: peer.to_string() }));
                ctx.notify(format!("{name} sent a prompt to this session"));
            }
            NodeEvent::AnswerRequested { peer, session, decision, values, reason } => {
                let answer = Answer { values, reason, by: peer.to_string() };
                let message = crate::areas::decisions::Message::Answer { session, decision, answer };
                ctx.request(Request::Forward(Box::new(crate::app::Message::Decisions(message))));
            }
        }
    }

    /// Opens the node again with the saved `[network]` settings.
    fn restart(&mut self, ctx: &mut Ctx) -> Task<Message> {
        self.remote = None;
        self.folder = None;
        if ctx.screen() == Some(AreaId::Peers) {
            ctx.close_screen();
        }
        match self.node.take() {
            Some(node) => Task::perform(async move { node.stop().await }, |()| Message::Stopped),
            None => Task::done(Message::Stopped),
        }
    }
}

impl Area for State {
    type Message = Message;

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Panel(message) => {
                let (task, event) = self.panel.update(message);
                let effect = event.map_or_else(Task::none, |event| self.apply(event, ctx));
                Task::batch([task.map(Message::Panel), effect])
            }
            Message::Open => {
                ctx.open_tab(TAB);
                self.refresh(ctx);
                self.resources(ctx)
            }
            Message::Refresh => {
                self.refresh(ctx);
                self.resources(ctx)
            }
            Message::Started(result) => {
                if let Err(error) = result {
                    ctx.notify(format!("The peer endpoint didn't start: {error}"));
                }
                self.refresh(ctx);
                Task::none()
            }
            Message::Node(event) => {
                self.node_event(event, ctx);
                Task::none()
            }
            Message::Applied(Ok(outcome)) => self.applied(outcome, ctx),
            Message::Applied(Err(error)) => {
                ctx.notify(error);
                self.refresh(ctx);
                Task::none()
            }
            Message::Resources(resources) => {
                if let Some(node) = &self.node {
                    node.set_resources(resources);
                }
                self.refresh(ctx);
                Task::none()
            }
            Message::Stopped => {
                let network = network_settings(ctx);
                let config = node_config(ctx, &network);
                self.open(ctx, config)
            }
            Message::OpenRecent(id) => {
                let Some((peer, session)) = parse_recent(&id) else { return Task::none() };
                if self.remote.as_ref().is_some_and(|remote| remote.is(peer, session) && remote.ended.is_none()) {
                    self.folder = None;
                    ctx.show_screen(AreaId::Peers);
                    return Task::none();
                }
                self.apply(peers::Event::OpenRemote { peer: peer.to_owned(), session: session.to_owned() }, ctx)
            }
            Message::ForgetRecent(_) => {
                ctx.notify("A peer's session is deleted on its own node; revoke or let the grant end to hide it");
                Task::none()
            }
            Message::Remote(message) => self.remote_update(message, ctx),
            Message::Folder(message) => self.folder_update(message, ctx),
        }
    }

    fn palette_commands(&self, _: &Ctx) -> Vec<palette::Command> {
        vec![
            palette::Command::new(AreaId::Peers.command(COPY_KEY), "Copy this node's key", "Peers"),
            palette::Command::new(AreaId::Peers.command(REFRESH), "Check peers now", "Peers"),
            palette::Command::new(AreaId::Peers.command(SCAN), "Scan the tailnet for nodes", "Peers"),
            palette::Command::new(AreaId::Peers.command(RESTART), "Restart the peer endpoint with the network settings", "Peers"),
        ]
    }

    fn run_command(&mut self, id: &str, ctx: &mut Ctx) -> Option<Task<Message>> {
        let name = id.strip_prefix(AreaId::Peers.key())?.strip_prefix('.')?;
        Some(match name {
            COPY_KEY => {
                ctx.notify("Key copied");
                iced::clipboard::write(ctx.services.node.key.clone())
            }
            REFRESH => self.apply(peers::Event::Refresh, ctx),
            SCAN => self.apply(peers::Event::ScanTailscale, ctx),
            RESTART => self.restart(ctx),
            _ => return None,
        })
    }

    fn panel_tabs(&self) -> Vec<PanelTab> {
        vec![PanelTab::new(TAB, "Peers", false)]
    }

    fn view_tab<'a>(&'a self, id: &str, _: &'a Ctx) -> Option<Element<'a, Message>> {
        (id == TAB).then(|| self.panel.view().map(Message::Panel))
    }

    fn view_screen<'a>(&'a self, _: &'a Ctx) -> Option<Element<'a, Message>> {
        if let Some(browser) = &self.folder {
            return Some(browser.view().map(Message::Folder));
        }
        self.remote.as_ref().map(|remote| remote.view().map(Message::Remote))
    }

    fn on_session_event(&mut self, event: &crate::area::SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        use crate::area::SessionEvent;
        // New sessions and prompts become resources grants can name.
        if matches!(event, SessionEvent::Loaded(_) | SessionEvent::TurnStarted { .. } | SessionEvent::Closed(_)) {
            self.refresh(ctx);
        }
        Task::none()
    }

    fn subscription(&self) -> Subscription<Message> {
        let events = self
            .node
            .as_ref()
            .map_or_else(Subscription::none, |node| Subscription::run_with(Events(node.clone(), self.generation), events).map(Message::Node));
        let remote = self.remote.as_ref().map_or_else(Subscription::none, |remote| remote.subscription().map(Message::Remote));
        Subscription::batch([events, remote])
    }
}

/// The node's event feed; a node opened again gets a new one.
pub(crate) struct Events(pub(crate) Node, pub(crate) u64);

impl Hash for Events {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.key().hash(state);
        self.1.hash(state);
    }
}

/// Every event of the node; a feed that fell behind reports a change.
pub(crate) fn events(events: &Events) -> impl Stream<Item = NodeEvent> + use<> {
    let node = events.0.clone();
    iced::stream::channel(64, async move |mut out: iced::futures::channel::mpsc::Sender<NodeEvent>| {
        let mut receiver = node.events();
        drop(node);
        loop {
            let event = match receiver.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => NodeEvent::Changed,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            };
            if out.send(event).await.is_err() {
                return;
            }
        }
    })
}
