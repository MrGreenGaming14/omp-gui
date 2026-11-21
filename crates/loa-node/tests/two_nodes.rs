//! Two nodes in one process over loopback (real iroh QUIC and TCP): peers,
//! presence, grants, and a shared session viewed, driven, prompted, forked
//! and revoked.

use loa_bus::{Bus, Persist, PersistError, SessionId, SessionWriter, WriterConfig};
use loa_core::{Dom, Draft, ElementKind, NodeId as DomNodeId, Patch, PatchSink, Value, attr, now_ms};
use loa_trust::{Action, GrantId, GrantStore, Resource, Scope};
use loa_trust::{KeySource, NodeIdentity};
use loa_bus::{JournalOptions, Store};
use loa_features::library::Library;
use loa_node::{BusSessions, DriveOutcome, GrantDirection, LibrarySessions, Node, NodeConfig, NodeEvent, Transport, ui};
use loa_ui_peers as panel;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

struct Discard;

impl Persist for Discard {
    fn append(&mut self, _: &Patch) -> Result<(), PersistError> {
        Ok(())
    }
}

struct Peer {
    node: Node,
    bus: Arc<Bus>,
    store: Store,
    _dir: tempfile::TempDir,
}

async fn peer(name: &str) -> Peer {
    peer_with(name, |_| {}).await
}

async fn peer_with(name: &str, configure: impl FnOnce(&mut NodeConfig)) -> Peer {
    let dir = tempfile::tempdir().unwrap();
    let bus = Arc::new(Bus::new());
    let store = Store::new(dir.path().join("sessions"), JournalOptions::default()).unwrap();
    let mut config = NodeConfig::new(dir.path().join("node"), name, "0.4.2");
    config.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    config.mdns = false;
    config.tailscale = None;
    config.presence_interval = Duration::from_secs(3600);
    config.sessions = Some(store.clone());
    configure(&mut config);
    let identity = NodeIdentity::generate().unwrap();
    let node = Node::open(config, &identity, KeySource::Keychain, Arc::new(BusSessions::new(Arc::clone(&bus)))).unwrap();
    node.start().await.unwrap();
    Peer { node, bus, store, _dir: dir }
}

fn addresses(node: &Node) -> (SocketAddr, SocketAddr) {
    match node.status().endpoint {
        loa_node::Endpoint::Listening { udp, tcp } => (udp[0], tcp),
        other => panic!("not listening: {other:?}"),
    }
}

/// A session this node owns, with one prompt and an answer.
fn session(owner: &Peer, id: &str) -> (Arc<SessionWriter>, DomNodeId) {
    let key = loa_bus::NodeKey(*owner.node.key().as_bytes());
    let writer = owner
        .bus
        .open(WriterConfig {
            session: SessionId::from(id),
            owner: key,
            local: key,
            dom: Dom::new(),
            persist: Box::new(Discard),
            authorize: owner.node.authorizer(),
        })
        .unwrap();
    let ts = now_ms();
    let root = writer
        .commit_at(
            Draft::insert(DomNodeId::DOCUMENT, ElementKind::Session, [
                (attr::ID, Value::from(id)),
                (attr::HARNESS, Value::from("omp")),
                (attr::NODE, Value::from(owner.node.key().to_string().as_str())),
                (attr::CREATED, Value::Int(ts)),
                (attr::SCHEMA, Value::Int(1)),
                (attr::TITLE, Value::from("Fix the parser")),
                (attr::PROJECT, Value::from("/home/owner/secret-project")),
            ], ""),
            ts,
        )
        .unwrap();
    let turn = writer.commit_at(Draft::insert(root.node, ElementKind::Turn, std::iter::empty::<(&str, Value)>(), ""), ts).unwrap();
    writer
        .commit_at(Draft::insert(turn.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], "Why does it fail?"), ts)
        .unwrap();
    let answer = writer
        .commit_at(Draft::insert(turn.node, ElementKind::Message, [(attr::ROLE, Value::from("assistant"))], "Because"), ts)
        .unwrap();
    (writer, answer.node)
}

async fn eventually(what: &str, mut check: impl FnMut() -> bool) {
    for _ in 0..200 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

fn text_of(dom: &Dom, role: &str) -> Option<String> {
    dom.iter().find(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some(role)).map(|n| n.text.clone())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_nodes_share_view_drive_fork_and_revoke_a_session() {
    let owner = peer("owner-box").await;
    let guest = peer("guest-box").await;
    let (owner_udp, _) = addresses(&owner.node);
    let (guest_udp, _) = addresses(&guest.node);

    // N1/N2: add by key; the probe records presence, transport, version and name.
    guest.node.add_peer(&owner.node.key().to_string(), Some(&owner_udp.to_string())).await.unwrap();
    owner.node.add_peer(&guest.node.key().to_string(), Some(&guest_udp.to_string())).await.unwrap();
    let seen = guest.node.peers();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].online);
    assert_eq!(seen[0].transport, Some(Transport::Quic));
    assert_eq!(seen[0].app_version.as_deref(), Some("0.4.2"));
    assert_eq!(seen[0].name.as_deref(), Some("owner-box"));
    assert_eq!(seen[0].shared_protocol, Some(true));
    assert_eq!(seen[0].protocols, loa_node::local_protocols());
    assert_eq!(loa_node::local_protocols(), ["omp/control/1", "omp/session/1", "omp/folder/1"]);

    let (writer, answer) = session(&owner, "s-shared");
    let mut owner_events = owner.node.events();

    // N3: the owner grants drive (and fork) on the session through the panel path.
    let input = panel::GrantInput {
        peer_key: guest.node.key().to_string(),
        resource: panel::GrantResource { id: "session:s-shared".into(), label: "Fix the parser".into(), scope: panel::GrantScope::Session, future_sessions: false },
        permission: panel::Permission::Drive,
        ttl_seconds: 3600,
        reshare: false,
    };
    ui::apply(owner.node.clone(), panel::Event::CreateGrant(input)).await.unwrap();
    let fork = Scope::new(Resource::Session { id: "s-shared".into() }, [Action::Fork]).unwrap();
    owner.node.issue_grant(&guest.node.key().to_string(), vec![fork], Some(now_ms() + 3_600_000), false).await.unwrap();
    let received: Vec<_> = guest.node.grants().into_iter().filter(|g| g.direction == GrantDirection::Received).collect();
    assert_eq!(received.len(), 2, "both grants were delivered over omp/control/1");
    assert!(received.iter().all(|g| g.peer == owner.node.key() && g.renews_at_ms.is_some()));

    // N4: the guest lists, then opens the session.
    let listed = guest.node.list_remote(&owner.node.key().to_string()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "Fix the parser");
    assert!(listed[0].actions.contains(&Action::Drive) && listed[0].actions.contains(&Action::Fork));
    assert_eq!(guest.node.remote_sessions().len(), 1, "peer sessions are kept for Recents");

    let view = guest.node.open_remote(&owner.node.key().to_string(), "s-shared").await.unwrap();
    assert!(view.can(Action::Drive) && view.can(Action::Fork));
    assert_eq!(view.source_seq(), writer.seq());
    view.writer().with_dom(|dom| {
        let root = dom.root().unwrap();
        assert_eq!(root.str_attr(attr::TITLE), Some("Fix the parser"));
        assert!(!root.attrs.contains_key(attr::PROJECT), "project paths stay with the owner");
        assert_eq!(text_of(dom, "assistant").as_deref(), Some("Because"));
    });

    // Live updates from the owner reach the view.
    writer.commit_at(Draft::append(answer, " the lexer drops tokens"), now_ms()).unwrap();
    eventually("the owner's patch on the view", || {
        view.writer().with_dom(|dom| text_of(dom, "assistant").as_deref() == Some("Because the lexer drops tokens"))
    })
    .await;

    // Drive: the guest edits through the owner, the single writer.
    let visible_answer = view.writer().with_dom(|dom| {
        dom.iter().find(|n| n.str_attr(attr::ROLE) == Some("assistant")).unwrap().id
    });
    let before = view.source_seq();
    let outcome = view.drive(Draft::append(visible_answer, ".")).await.unwrap();
    assert_eq!(outcome, DriveOutcome::Applied { source_seq: before + 1 });
    writer.with_dom(|dom| assert_eq!(text_of(dom, "assistant").as_deref(), Some("Because the lexer drops tokens.")));
    eventually("the driven patch back on the view", || view.source_seq() == before + 1).await;

    // A request made on an old revision is refused, not applied.
    writer.commit_at(Draft::append(answer, "!"), now_ms()).unwrap();
    let stale = view.drive_at(before, Draft::append(visible_answer, "?")).await.unwrap();
    assert_eq!(stale, DriveOutcome::Conflict { current: before + 2 });
    writer.with_dom(|dom| assert!(!text_of(dom, "assistant").unwrap().ends_with('?')));

    // A prompt goes to the owner app for its harness.
    view.prompt("Now write a test").await.unwrap();
    let requested = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(NodeEvent::PromptRequested { peer, session, text }) = owner_events.recv().await {
                break (peer, session, text);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(requested, (guest.node.key(), "s-shared".to_owned(), "Now write a test".to_owned()));

    // Fork: a copy owned by the guest, in the guest's store.
    let forked = guest.node.fork_remote(&owner.node.key().to_string(), "s-shared").await.unwrap();
    let (journal, _) = guest.store.open(&forked).unwrap();
    let dom = journal.load().unwrap();
    let root = dom.root().unwrap();
    assert_eq!(root.str_attr(attr::NODE), Some(guest.node.key().to_string().as_str()));
    assert_eq!(root.str_attr(attr::PARENT_SESSION), Some("s-shared"));
    assert_eq!(text_of(&dom, "user").as_deref(), Some("Why does it fail?"));
    drop(journal);

    // The panel snapshot shows the shared session and the online peer.
    let data = ui::data(&guest.node, now_ms());
    assert_eq!(data.remote_sessions.len(), 1);
    assert!(data.remote_sessions[0].online && data.remote_sessions[0].can_drive && data.remote_sessions[0].can_fork);
    assert_eq!(data.peers[0].presence, panel::Presence::Online);
    assert_eq!(data.peers[0].transport, panel::Transport::Direct);
    assert!(data.node.endpoint_ok);

    // Revoking every grant stops the view at the owner's next patch and tells the guest.
    let given: Vec<GrantId> = owner.node.grants().into_iter().filter(|g| g.direction == GrantDirection::Given).map(|g| g.id).collect();
    for id in &given {
        owner.node.revoke_grant(id).await.unwrap();
    }
    writer.commit_at(Draft::append(answer, " after revoke"), now_ms()).unwrap();
    eventually("the view to end", || view.ended().is_some()).await;
    view.writer().with_dom(|dom| assert!(!text_of(dom, "assistant").unwrap().contains("after revoke")));
    assert!(guest.node.grants().iter().filter(|g| g.direction == GrantDirection::Received).all(|g| g.ended));
    assert!(guest.node.open_remote(&owner.node.key().to_string(), "s-shared").await.is_err());

    guest.node.stop().await;
    owner.node.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn view_only_grants_refuse_drive_and_hide_other_prompts() {
    let owner = peer("owner").await;
    let guest = peer("guest").await;
    let (owner_udp, _) = addresses(&owner.node);
    guest.node.add_peer(&owner.node.key().to_string(), Some(&owner_udp.to_string())).await.unwrap();
    let (writer, _) = session(&owner, "s-view");
    let root = writer.with_dom(|dom| dom.root().unwrap().id);
    let ts = now_ms();
    let second = writer.commit_at(Draft::insert(root, ElementKind::Turn, std::iter::empty::<(&str, Value)>(), ""), ts).unwrap();
    writer.commit_at(Draft::insert(second.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], "only this one"), ts).unwrap();

    let prompt = Scope::new(Resource::Prompt { session: "s-view".into(), turn: second.node }, [Action::View]).unwrap();
    owner.node.issue_grant(&guest.node.key().to_string(), vec![prompt], None, false).await.unwrap();
    let listed = guest.node.list_remote(&owner.node.key().to_string()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].partial && listed[0].actions.is_empty() && listed[0].title.is_empty());

    let view = guest.node.open_remote(&owner.node.key().to_string(), "s-view").await.unwrap();
    view.writer().with_dom(|dom| {
        let messages: Vec<_> = dom.iter().filter(|n| n.kind == ElementKind::Message).map(|n| n.text.clone()).collect();
        assert_eq!(messages, ["only this one"]);
    });
    let message = view.writer().with_dom(|dom| dom.iter().find(|n| n.kind == ElementKind::Message).unwrap().id);
    assert!(view.drive(Draft::append(message, "!")).await.is_err(), "view-only grants can't drive");
    assert!(view.prompt("hi").await.is_err());
    assert!(guest.node.fork_remote(&owner.node.key().to_string(), "s-view").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_blocked_peer_is_reached_over_tcp_fallback() {
    let owner = peer("owner").await;
    let guest = peer("guest").await;
    let (_, owner_tcp) = addresses(&owner.node);
    // Only the TCP address: QUIC to it gets no answer, the TCP fallback connects.
    guest.node.add_peer(&owner.node.key().to_string(), Some(&owner_tcp.to_string())).await.unwrap();
    let seen = guest.node.peers();
    assert!(seen[0].online);
    assert_eq!(seen[0].transport, Some(Transport::Tcp));
    let data = ui::data(&guest.node, now_ms());
    assert_eq!(data.peers[0].transport, panel::Transport::TcpFallback);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unreachable_peers_go_offline_and_grants_wait_for_them() {
    let owner = peer("owner").await;
    let guest = peer("guest").await;
    let (guest_udp, _) = addresses(&guest.node);
    let guest_key = guest.node.key().to_string();
    owner.node.add_peer(&guest_key, Some(&guest_udp.to_string())).await.unwrap();
    assert!(owner.node.peers()[0].online);
    guest.node.stop().await;
    owner.node.refresh_presence().await;
    assert!(!owner.node.peers()[0].online, "a peer that stops answering is offline");

    let scope = Scope::new(Resource::FutureSessions { after_ms: now_ms() }, [Action::View]).unwrap();
    let id = owner.node.issue_grant(&guest_key, vec![scope], None, false).await.unwrap();
    assert!(guest.node.grants().is_empty());
    guest.node.start().await.unwrap();
    // The guest restarted on new ports; the owner learns them like a user would.
    let (guest_udp, _) = addresses(&guest.node);
    owner.node.remove_peer(&guest_key).unwrap();
    owner.node.add_peer(&guest_key, Some(&guest_udp.to_string())).await.unwrap();
    let received = guest.node.grants();
    assert_eq!(received.len(), 1, "the waiting grant was delivered once the holder answered");
    assert_eq!(received[0].id, id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn held_grants_renew_from_their_issuer_and_stop_after_revocation() {
    let owner = peer("owner").await;
    // A margin wider than the 10 min token life: every held token is due at once,
    // as it is for the background loop 3 min before expiry.
    let guest = peer_with("guest", |config| config.renew_margin_ms = 20 * 60_000).await;
    let (owner_udp, _) = addresses(&owner.node);
    let (guest_udp, _) = addresses(&guest.node);
    guest.node.add_peer(&owner.node.key().to_string(), Some(&owner_udp.to_string())).await.unwrap();
    owner.node.add_peer(&guest.node.key().to_string(), Some(&guest_udp.to_string())).await.unwrap();
    let scope = Scope::new(Resource::Session { id: "s".into() }, [Action::View]).unwrap();
    owner.node.issue_grant(&guest.node.key().to_string(), vec![scope], None, false).await.unwrap();
    let first = guest.node.grants()[0].expires_ms.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let outcomes = guest.node.renew_due().await;
    assert!(matches!(outcomes.as_slice(), [loa_trust::RenewOutcome::Renewed { .. }]), "{outcomes:?}");
    assert!(guest.node.grants()[0].expires_ms.unwrap() > first);

    let id = owner.node.grants()[0].id.clone();
    owner.node.revoke_grant(&id).await.unwrap();
    assert!(guest.node.grants()[0].ended);
    assert!(guest.node.renew_due().await.is_empty(), "a refused grant stops renewing");
}

/// The app's indirection: the library's writers ask the node, which exists only after the library.
#[derive(Default)]
struct Late(std::sync::OnceLock<Arc<dyn loa_bus::Authorize>>);

impl loa_bus::Authorize for Late {
    fn authorize(&self, session: &SessionId, requester: &loa_bus::NodeKey, draft: &Draft, dom: &Dom) -> Result<(), loa_bus::Denied> {
        match self.0.get() {
            Some(authorize) => authorize.authorize(session, requester, draft, dom),
            None => Err(loa_bus::Denied("no node yet".into())),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn journaled_library_sessions_are_listed_viewed_and_driven_by_peers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("sessions"), JournalOptions::default()).unwrap();
    let identity = NodeIdentity::generate().unwrap();
    let key = loa_bus::NodeKey(*identity.public_key().as_bytes());
    let late = Arc::new(Late::default());
    let library = Arc::new(Library::new(store.clone(), Arc::new(Bus::new()), key, Arc::clone(&late) as Arc<dyn loa_bus::Authorize>));
    let mut config = NodeConfig::new(dir.path().join("node"), "owner", "0.4.2");
    config.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    config.mdns = false;
    config.tailscale = None;
    config.presence_interval = Duration::from_secs(3600);
    config.sessions = Some(store.clone());
    // The app's one grant store, outside the node's directory.
    let grants = Arc::new(GrantStore::open(&dir.path().join("grants.db")).unwrap());
    let sessions = Arc::new(LibrarySessions::new(Arc::clone(&library)));
    let owner = Node::open_with_grants(config, &identity, KeySource::Keychain, sessions, Arc::clone(&grants)).unwrap();
    assert!(late.0.set(owner.authorizer()).is_ok());
    owner.start().await.unwrap();

    // A session written, then closed: it lives only in its journal.
    let live = library.create("s-journaled").unwrap();
    let ts = now_ms();
    let root = live
        .writer
        .commit_at(
            Draft::insert(DomNodeId::DOCUMENT, ElementKind::Session, [
                (attr::ID, Value::from("s-journaled")),
                (attr::HARNESS, Value::from("omp")),
                (attr::NODE, Value::from(owner.key().to_string().as_str())),
                (attr::CREATED, Value::Int(ts)),
                (attr::SCHEMA, Value::Int(1)),
                (attr::TITLE, Value::from("Journaled")),
            ], ""),
            ts,
        )
        .unwrap();
    let turn = live.writer.commit_at(Draft::insert(root.node, ElementKind::Turn, std::iter::empty::<(&str, Value)>(), ""), ts).unwrap();
    live.writer.commit_at(Draft::insert(turn.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], "hello"), ts).unwrap();
    drop(live);
    assert!(library.close("s-journaled").unwrap());
    assert!(owner.resources().iter().any(|(_, label)| label == "Journaled"), "closed sessions are grant resources");

    let guest = peer("guest").await;
    let (owner_udp, _) = addresses(&owner);
    guest.node.add_peer(&owner.key().to_string(), Some(&owner_udp.to_string())).await.unwrap();
    let scope = Scope::new(Resource::Session { id: "s-journaled".into() }, [Action::Drive]).unwrap();
    owner.issue_grant(&guest.node.key().to_string(), vec![scope], None, false).await.unwrap();
    assert_eq!(grants.issued().unwrap().len(), 1, "the node writes into the shared store");
    let listed = guest.node.list_remote(&owner.key().to_string()).await.unwrap();
    assert_eq!(listed.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["Journaled"]);

    let view = guest.node.open_remote(&owner.key().to_string(), "s-journaled").await.unwrap();
    let message = view.writer().with_dom(|dom| dom.iter().find(|n| n.kind == ElementKind::Message).unwrap().id);
    assert!(matches!(view.drive(Draft::append(message, " there")).await.unwrap(), DriveOutcome::Applied { .. }));
    // The owner journals the peer's change like its own.
    library.close("s-journaled").unwrap();
    let (journal, _) = store.open("s-journaled").unwrap();
    assert_eq!(text_of(&journal.load().unwrap(), "user").as_deref(), Some("hello there"));
}

/// Echoes every byte back on the stream.
struct Echo;

#[async_trait::async_trait]
impl loa_node::PeerProtocol for Echo {
    async fn serve(&self, _peer: loa_net::NodeId, mut stream: loa_node::Stream) {
        let _ = tokio::io::copy(&mut stream.recv, &mut stream.send).await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream.send).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn added_protocols_serve_only_peers_with_a_covering_grant() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let owner = peer("owner-box").await;
    let guest = peer("guest-box").await;
    let required = (Resource::Sandbox, Action::Exec);
    owner.node.add_protocol("loa-echo", 1, required.clone(), Arc::new(Echo));
    owner.node.stop().await;
    owner.node.start().await.unwrap();
    assert!(owner.node.resources().iter().any(|(resource, label)| *resource == Resource::Sandbox && label == loa_node::SANDBOX_LABEL));
    let (owner_udp, _) = addresses(&owner.node);
    let (guest_udp, _) = addresses(&guest.node);
    guest.node.add_peer(&owner.node.key().to_string(), Some(&owner_udp.to_string())).await.unwrap();
    owner.node.add_peer(&guest.node.key().to_string(), Some(&guest_udp.to_string())).await.unwrap();

    // Without a grant the owner refuses the stream.
    assert!(guest.node.granting_peers(&required).is_empty());
    let refused = guest.node.open_stream(owner.node.key(), "loa-echo", 1).await;
    assert!(matches!(refused, Err(loa_node::NodeError::Refused(_))), "{:?}", refused.err());

    // A grant on another resource doesn't cover it either.
    let ssh = Scope::new(Resource::Ssh { host: "box".into() }, [Action::Exec]).unwrap();
    owner.node.issue_grant(&guest.node.key().to_string(), vec![ssh], None, false).await.unwrap();
    assert!(guest.node.open_stream(owner.node.key(), "loa-echo", 1).await.is_err());

    // A sandbox grant does: the handler gets the stream.
    let sandbox = Scope::new(Resource::Sandbox, [Action::Exec]).unwrap();
    let id = owner.node.issue_grant(&guest.node.key().to_string(), vec![sandbox], None, false).await.unwrap();
    eventually("the sandbox grant at the guest", || guest.node.granting_peers(&required) == [owner.node.key()]).await;
    let mut stream = guest.node.open_stream(owner.node.key(), "loa-echo", 1).await.unwrap();
    stream.send.write_all(b"ping").await.unwrap();
    stream.send.shutdown().await.unwrap();
    let mut echoed = Vec::new();
    stream.recv.read_to_end(&mut echoed).await.unwrap();
    assert_eq!(echoed, b"ping");

    // Revoked: refused again.
    owner.node.revoke_grant(&id).await.unwrap();
    assert!(guest.node.open_stream(owner.node.key(), "loa-echo", 1).await.is_err());
    guest.node.stop().await;
    owner.node.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn folder_grants_list_read_and_write_only_inside_the_folder() {
    let owner = peer("owner-box").await;
    let guest = peer("guest-box").await;
    let (owner_udp, _) = addresses(&owner.node);
    let (guest_udp, _) = addresses(&guest.node);
    guest.node.add_peer(&owner.node.key().to_string(), Some(&owner_udp.to_string())).await.unwrap();
    owner.node.add_peer(&guest.node.key().to_string(), Some(&guest_udp.to_string())).await.unwrap();
    let files = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(files.path()).unwrap();
    let share = base.join("share");
    std::fs::create_dir_all(share.join("docs")).unwrap();
    std::fs::write(base.join("secret.txt"), "outside").unwrap();
    // More than one 512 KiB frame.
    let big: Vec<u8> = (0..1_300_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(share.join("docs/big.bin"), &big).unwrap();
    std::fs::write(share.join("notes.txt"), "hello").unwrap();
    let folder = share.to_str().unwrap().to_owned();
    let owner_key = owner.node.key().to_string();

    // No grant: refused.
    assert!(matches!(guest.node.list_folder(&owner_key, &folder, "").await, Err(loa_node::NodeError::Refused(_))));

    // A read grant: list, read (several frames), no writes, nothing outside.
    let read = Scope::new(Resource::Folder { path: folder.clone() }, [Action::Read]).unwrap();
    let id = owner.node.issue_grant(&guest.node.key().to_string(), vec![read], None, false).await.unwrap();
    eventually("the folder grant at the guest", || !guest.node.shared_folders().is_empty()).await;
    let shared = guest.node.shared_folders();
    assert_eq!(shared, [loa_node::SharedFolder { peer: owner.node.key(), path: folder.clone(), can_write: false }]);
    let entries = guest.node.list_folder(&owner_key, &folder, "").await.unwrap();
    let names: Vec<(&str, bool)> = entries.iter().map(|e| (e.name.as_str(), e.dir)).collect();
    assert_eq!(names, [("docs", true), ("notes.txt", false)]);
    assert_eq!(guest.node.read_folder_file(&owner_key, &folder, "docs/big.bin").await.unwrap(), big);
    assert_eq!(guest.node.list_folder(&owner_key, &format!("{folder}/docs"), "").await.unwrap()[0].size, big.len() as u64);
    for escape in ["../secret.txt", "/etc/hostname", "docs/../../secret.txt"] {
        assert!(guest.node.read_folder_file(&owner_key, &folder, escape).await.is_err(), "{escape}");
    }
    assert!(guest.node.list_folder(&owner_key, base.to_str().unwrap(), "").await.is_err(), "the folder above");
    let write = guest.node.write_folder_file(&owner_key, &folder, "notes.txt", b"changed").await;
    assert!(matches!(&write, Err(loa_node::NodeError::Refused(error)) if error.contains("no grant")), "{write:?}");
    assert_eq!(std::fs::read_to_string(share.join("notes.txt")).unwrap(), "hello");

    // A write grant writes, in several frames too.
    let write = Scope::new(Resource::Folder { path: folder.clone() }, [Action::Write]).unwrap();
    owner.node.issue_grant(&guest.node.key().to_string(), vec![write], None, false).await.unwrap();
    eventually("the write grant at the guest", || guest.node.shared_folders().first().is_some_and(|f| f.can_write)).await;
    guest.node.write_folder_file(&owner_key, &folder, "docs/copy.bin", &big).await.unwrap();
    assert_eq!(std::fs::read(share.join("docs/copy.bin")).unwrap(), big);
    guest.node.write_folder_file(&owner_key, &folder, "empty.txt", b"").await.unwrap();
    assert_eq!(std::fs::read(share.join("empty.txt")).unwrap(), b"");
    assert!(guest.node.write_folder_file(&owner_key, &folder, "../escaped.txt", b"x").await.is_err());
    assert!(!base.join("escaped.txt").exists());
    assert!(owner.node.resources().iter().any(|(resource, label)| *resource == Resource::Folder { path: folder.clone() } && *label == format!("Folder {folder}")));

    // Revoking the read grant leaves the write grant working; revoking it too ends access.
    owner.node.revoke_grant(&id).await.unwrap();
    assert!(guest.node.list_folder(&owner_key, &folder, "").await.is_ok());
    let given: Vec<GrantId> = owner.node.grants().into_iter().filter(|g| g.direction == GrantDirection::Given && !g.ended).map(|g| g.id).collect();
    for id in given {
        owner.node.revoke_grant(&id).await.unwrap();
    }
    assert!(guest.node.list_folder(&owner_key, &folder, "").await.is_err());
    guest.node.stop().await;
    owner.node.stop().await;
}
