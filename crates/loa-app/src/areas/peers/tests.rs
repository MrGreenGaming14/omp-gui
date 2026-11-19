//! Two apps in one process, each with its own node over loopback (real iroh
//! QUIC and TCP), driven through `App::update`: adding peers, grants issued,
//! renewed and revoked in the Peers panel, a peer's session in Recents
//! tagged by its node, opened, prompted (a drive request the owner's harness
//! runs) and forked.

use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::Task;
use iced::futures::StreamExt;
use iced_runtime::Action as RuntimeAction;
use loa_core::{Dom, Draft, ElementKind, attr};
use loa_identity::NodeIdentity;
use loa_node::{DriveOutcome, Endpoint, GrantDirection, NodeEvent};
use loa_ui_peers as panel;
use loa_ui_shell as shell;
use tokio::runtime::Runtime;

use super::{Feed, Message as Peers, RemoteMessage, is_remote, recent_id};
use crate::app::tests::{FakeAdapter, active};
use crate::app::{App, Message};
use crate::services::{Dirs, Harnesses, Node, Services};

/// An app on its own key without mDNS or tailnet scans, whose node listens
/// on a free port: `[network] port` can't be 0, so the node is opened again
/// over the saved settings with port 0.
fn app(dir: &std::path::Path, name: &str) -> App {
    let dirs = Dirs::new(dir.join("loa"));
    let config = loa_config::ConfigStore::in_dir(&dirs.data);
    let mut settings = config.load().unwrap().config;
    settings.network.discovery.mdns = false;
    settings.network.discovery.tailscale = false;
    config.save(&settings).unwrap();
    let mut harnesses = Harnesses { registry: loa_adapter::Registry::new(), list: Vec::new() };
    harnesses.add(Arc::new(FakeAdapter::new("omp")), "omp", true, None, None);
    let node = Node::new(NodeIdentity::generate().unwrap(), name.to_owned());
    let mut app = App::build(Services::new(node, dirs, harnesses, dir.to_owned()).unwrap()).0;
    settings.network.port = 0;
    let config = super::node_config(&app.ctx, &settings.network);
    // The startup task (which would start the first node) is dropped; `start` starts this one.
    app.areas.peers = super::State::with_config(&mut app.ctx, config).0;
    app
}

/// Runs `task` on `rt` as the iced runtime would, feeding each message back
/// through `App::update`. The node's connections live on `rt`, so they
/// outlast each call.
fn run(rt: &Runtime, app: &mut App, task: Task<Message>) {
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        let Some(stream) = iced_runtime::task::into_stream(task) else { continue };
        let outputs = rt.block_on(async {
            let outputs = stream.filter_map(|action| async move {
                match action {
                    RuntimeAction::Output(message) => Some(message),
                    _ => None,
                }
            });
            tokio::time::timeout(Duration::from_secs(20), outputs.collect::<Vec<_>>()).await.expect("task finished")
        });
        for message in outputs {
            pending.push(app.update(message));
        }
    }
}

fn update(rt: &Runtime, app: &mut App, message: Message) {
    let task = app.update(message);
    run(rt, app, task);
}

fn panel(rt: &Runtime, app: &mut App, message: panel::Message) {
    update(rt, app, Message::Peers(Peers::Panel(message)));
}

/// Starts the app's node, as the startup task does; returns its UDP port.
fn start(rt: &Runtime, app: &mut App) -> u16 {
    let node = app.areas.peers.node().expect("node opened").clone();
    let started = rt.block_on(node.start()).map_err(|error| error.to_string());
    update(rt, app, Message::Peers(Peers::Started(started)));
    match node.status().endpoint {
        Endpoint::Listening { udp, .. } => udp[0].port(),
        other => panic!("not listening: {other:?}"),
    }
}

fn key(app: &App) -> String {
    app.ctx.services.node.key.clone()
}

/// Adds `other` by key and loopback address through the panel's form.
fn add_peer(rt: &Runtime, app: &mut App, other: &App, port: u16) {
    let key = key(other);
    panel(rt, app, panel::Message::KeyChanged(key.clone()));
    // The key is checked first; the form only adds a valid key.
    panel(rt, app, panel::Message::AddressChanged(format!("127.0.0.1:{port}")));
    panel(rt, app, panel::Message::AddPeer);
}

/// Delivers the node events `events` holds as the area's feed would.
fn pump(rt: &Runtime, app: &mut App, events: &mut tokio::sync::broadcast::Receiver<NodeEvent>) {
    while let Ok(event) = events.try_recv() {
        update(rt, app, Message::Peers(Peers::Node(event)));
    }
}

fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn messages(dom: &Dom, role: &str) -> Vec<String> {
    dom.iter()
        .filter(|node| node.kind == ElementKind::Message && node.str_attr(attr::ROLE) == Some(role))
        .map(|node| node.text.clone())
        .collect()
}

/// Types `text` into the composer and submits it.
fn prompt(rt: &Runtime, app: &mut App, text: &str) {
    use iced::widget::text_editor::{Action, Edit};
    use loa_ui_composer as composer;
    for c in text.chars() {
        update(rt, app, Message::Composer(composer::Message::Edit(Action::Edit(Edit::Insert(c)))));
    }
    update(rt, app, Message::Composer(composer::Message::Submit));
}

/// Issues `permission` on `resource` to `holder` through the grant form.
fn grant(rt: &Runtime, app: &mut App, holder: &str, resource: &str, permission: panel::Permission) {
    // The user opens the Peers tab, which shows the current sessions.
    update(rt, app, Message::Peers(Peers::Open));
    panel(rt, app, panel::Message::SelectPeer(holder.to_owned()));
    panel(rt, app, panel::Message::SelectResource(resource.to_owned()));
    panel(rt, app, panel::Message::SelectPermission(permission));
    panel(rt, app, panel::Message::TtlChanged("2".into()));
    panel(rt, app, panel::Message::SubmitGrant);
}

#[test]
fn two_apps_share_a_session_view_drive_and_fork_over_loopback() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().unwrap();
    let _runtime = rt.enter();
    let (owner_dir, guest_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut owner = app(owner_dir.path(), "owner-box");
    let mut guest = app(guest_dir.path(), "guest-box");
    let owner_port = start(&rt, &mut owner);
    let guest_port = start(&rt, &mut guest);
    assert_ne!(owner_port, 0);

    // N1/N2: each adds the other by key; presence, transport and version show.
    add_peer(&rt, &mut guest, &owner, owner_port);
    add_peer(&rt, &mut owner, &guest, guest_port);
    let seen = &guest.ctx.peers;
    assert_eq!(seen.len(), 1, "{:?}", guest.ctx.notice());
    assert!(seen[0].online);
    assert_eq!(seen[0].transport, Some(loa_node::Transport::Quic));
    assert_eq!(seen[0].app_version.as_deref(), Some(env!("CARGO_PKG_VERSION")));
    let nodes = guest.ctx.shell.sidebar().nodes();
    assert_eq!(nodes.name_of(&key(&owner)), "owner-box");
    assert!(nodes.is_online(&key(&owner)));

    // The owner runs a session on its harness.
    update(&rt, &mut owner, Message::NewSession);
    prompt(&rt, &mut owner, "hello");
    let session = active(&owner).expect("owner session").to_owned();

    // N3: the owner grants drive and fork on it from the panel; the guest receives both.
    let guest_key = key(&guest);
    grant(&rt, &mut owner, &guest_key, &format!("session:{session}"), panel::Permission::Drive);
    grant(&rt, &mut owner, &guest_key, &format!("session:{session}"), panel::Permission::Fork);
    let owner_node = owner.areas.peers.node().unwrap().clone();
    let given: Vec<_> = owner_node.grants().into_iter().filter(|grant| grant.direction == GrantDirection::Given).collect();
    assert_eq!(given.len(), 2, "{:?}", owner.ctx.notice());
    let guest_node = guest.areas.peers.node().unwrap().clone();
    eventually("both grants on the guest", || {
        guest_node.grants().iter().filter(|grant| grant.direction == GrantDirection::Received).count() == 2
    });

    // SSH hosts in `[remote_hosts]` are grant resources; an SSH grant needs an explicit confirmation.
    let config = &owner.ctx.services.config;
    let mut settings = config.load().unwrap().config;
    settings.remote_hosts.insert("buildbox".into(), serde_json::from_value(serde_json::json!({"host": "buildbox.lan"})).unwrap());
    config.save(&settings).unwrap();
    update(&rt, &mut owner, Message::Peers(Peers::Open));
    let ssh = loa_grants::Resource::Ssh { host: "buildbox".into() };
    assert!(owner_node.resources().iter().any(|(resource, _)| *resource == ssh));
    panel(&rt, &mut owner, panel::Message::SelectPeer(guest_key.clone()));
    panel(&rt, &mut owner, panel::Message::SelectResource("ssh:buildbox".into()));
    panel(&rt, &mut owner, panel::Message::SelectPermission(panel::Permission::Drive));
    panel(&rt, &mut owner, panel::Message::SubmitGrant);
    assert_eq!(owner_node.grants().len(), 2, "not without the confirmation");
    panel(&rt, &mut owner, panel::Message::RequestSshConfirmation);
    panel(&rt, &mut owner, panel::Message::ConfirmSsh);
    panel(&rt, &mut owner, panel::Message::SubmitGrant);
    let given: Vec<_> = owner_node.grants().into_iter().filter(|grant| grant.direction == GrantDirection::Given).collect();
    assert!(given.iter().any(|grant| grant.scopes.iter().any(|scope| scope.resource == ssh)), "{:?}", owner.ctx.notice());
    assert_eq!(given.len(), 3);

    // N4: the guest's Refresh lists the session in Recents, tagged with the owner's node.
    panel(&rt, &mut guest, panel::Message::Refresh);
    let recent = recent_id(&key(&owner), &session);
    assert!(is_remote(&recent));
    let summary = guest.ctx.shell.sidebar().session(&recent).expect("peer session in Recents").clone();
    assert_eq!(summary.node, key(&owner));
    assert_eq!(summary.title, "hello");

    // Opening it from Recents shows the owner's transcript in the center.
    update(&rt, &mut guest, Message::Shell(shell::Message::Sidebar(shell::sidebar::Message::OpenSession(recent.clone()))));
    assert_eq!(guest.ctx.screen(), Some(crate::area::AreaId::Peers));
    let remote = guest.areas.peers.remote().expect("remote view");
    assert_eq!(messages(remote.dom(), "user"), ["hello"]);
    let view = Arc::clone(remote.view_handle());

    // A prompt from the guest is a drive request; the owner's harness runs it.
    let mut owner_events = owner_node.events();
    update(&rt, &mut guest, Message::Peers(Peers::Remote(RemoteMessage::Input("from the guest".into()))));
    update(&rt, &mut guest, Message::Peers(Peers::Remote(RemoteMessage::Send)));
    eventually("the prompt request at the owner", || !owner_events.is_empty());
    pump(&rt, &mut owner, &mut owner_events);
    let owned = owner.ctx.sessions.get(&session).expect("owner session open");
    owned.writer().with_dom(|dom| {
        assert_eq!(messages(dom, "user"), ["hello", "from the guest"]);
        assert_eq!(messages(dom, "assistant").len(), 2);
    });

    // The guest's mirror follows; its screen copy catches up from the feed.
    eventually("the owner's reply on the mirror", || view.writer().with_dom(|dom| messages(dom, "assistant").len() == 2));
    let mirror = view.writer().with_dom(Dom::clone);
    update(&rt, &mut guest, Message::Peers(Peers::Remote(RemoteMessage::Feed(Feed::Synced(Arc::new(mirror))))));
    assert_eq!(messages(guest.areas.peers.remote().unwrap().dom(), "user"), ["hello", "from the guest"]);

    // The owner stays the single writer: a drive on an old revision is refused.
    let message = view.writer().with_dom(|dom| dom.iter().find(|node| node.kind == ElementKind::Message).unwrap().id);
    let stale = rt.block_on(view.drive_at(1, Draft::append(message, "?"))).unwrap();
    assert!(matches!(stale, DriveOutcome::Conflict { .. }), "{stale:?}");
    owned.writer().with_dom(|dom| assert_eq!(messages(dom, "user")[0], "hello"));

    // Fork to the guest's node: a new local session with the transcript.
    update(&rt, &mut guest, Message::Peers(Peers::Remote(RemoteMessage::Fork)));
    let forked = active(&guest).expect("forked session active").to_owned();
    assert_ne!(forked, session);
    let copy = guest.ctx.sessions.get(&forked).unwrap().writer().with_dom(Dom::clone);
    assert_eq!(messages(&copy, "user"), ["hello", "from the guest"]);

    // N3: renew and revoke from the owner's panel; the guest loses the session.
    let drive = given.iter().find(|grant| grant.scopes.iter().any(|scope| scope.actions.contains(&loa_grants::Action::Drive))).unwrap();
    let until = drive.until_ms.expect("a timed grant");
    std::thread::sleep(Duration::from_millis(5));
    panel(&rt, &mut owner, panel::Message::RenewGrant(drive.id.to_string()));
    let renewed = owner_node.grants().into_iter().find(|grant| grant.id == drive.id).unwrap();
    assert!(renewed.until_ms.unwrap() > until, "{:?}", owner.ctx.notice());
    for grant in &given {
        panel(&rt, &mut owner, panel::Message::BeginRevoke(grant.id.to_string()));
        panel(&rt, &mut owner, panel::Message::ConfirmRevoke);
    }
    assert!(owner_node.grants().iter().filter(|grant| grant.direction == GrantDirection::Given).all(|grant| grant.ended));
    eventually("the revocation at the guest", || {
        guest_node.grants().iter().filter(|grant| grant.direction == GrantDirection::Received).all(|grant| grant.ended)
    });
    panel(&rt, &mut guest, panel::Message::Refresh);
    assert!(guest.ctx.shell.sidebar().session(&recent).is_none(), "revoked session leaves Recents");

    rt.block_on(owner_node.stop());
    rt.block_on(guest_node.stop());
}

/// A protocol another area adds; never dialed here.
struct Idle;

#[async_trait::async_trait]
impl loa_node::PeerProtocol for Idle {
    async fn serve(&self, _: loa_node::NodeId, _: loa_node::Stream) {}
}

#[test]
fn restart_command_listens_on_the_configured_port_and_keeps_added_protocols() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let _runtime = rt.enter();
    let dir = tempfile::tempdir().unwrap();
    let mut app = app(dir.path(), "box");
    // Another area (the sandbox) adds its protocol through the shared slot.
    let slot = app.ctx.node.clone();
    let first = slot.get().expect("node published");
    slot.add_protocol(loa_node::AddedProtocol {
        name: "loa-sandbox",
        version: 1,
        required: (loa_grants::Resource::Sandbox, loa_grants::Action::Exec),
        handler: Arc::new(Idle),
    });
    assert_eq!(first.added_protocols().len(), 1);
    start(&rt, &mut app);
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let config = &app.ctx.services.config;
    let mut settings = config.load().unwrap().config;
    settings.network.port = free;
    config.save(&settings).unwrap();
    let task = app.command(&crate::area::AreaId::Peers.command(super::RESTART));
    run(&rt, &mut app, task);
    let node = app.areas.peers.node().unwrap().clone();
    match node.status().endpoint {
        Endpoint::Listening { tcp, .. } => assert_eq!(tcp.port(), free),
        other => panic!("not listening: {other:?}"),
    }
    // The slot shows the new node, which serves the added protocol and
    // offers the sandbox as a grant resource.
    assert_eq!(slot.get().unwrap().status().endpoint, node.status().endpoint);
    assert_eq!(node.added_protocols().iter().map(|added| added.name).collect::<Vec<_>>(), ["loa-sandbox"]);
    assert!(node.resources().iter().any(|(resource, _)| *resource == loa_grants::Resource::Sandbox));
    rt.block_on(node.stop());
}
