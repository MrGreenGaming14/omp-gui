//! Two nodes in one process over loopback (real iroh QUIC and TCP): peers,
//! presence, grants, shared sessions, added protocols and folders.

use loa_bus::{Bus, JournalOptions, Persist, PersistError, SessionId, SessionWriter, Store, WriterConfig};
use loa_core::{Dom, Draft, ElementKind, NodeId as DomNodeId, Patch, PatchSink, Value, attr, now_ms};
use loa_harness::features::library::Library;
use loa_p2p::{BusSessions, DriveOutcome, GrantDirection, LibrarySessions, LocalSessions, Node, NodeConfig, NodeError, NodeEvent, TransportKind};
use loa_trust::{Action, GrantId, GrantStore, KeySource, NodeIdentity, Resource, Scope};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

struct Discard;

impl Persist for Discard {
    fn append(&mut self, _: &Patch) -> Result<(), PersistError> { Ok(()) }
}

struct Peer {
    node: Node,
    bus: Arc<Bus>,
    store: Store,
    _dir: tempfile::TempDir,
}

fn config(dir: &std::path::Path, name: &str, store: &Store) -> NodeConfig {
    let mut config = NodeConfig::new(dir.join("node"), name, "0.4.2");
    config.bind = SocketAddr::from(([127, 0, 0, 1], 0));
    config.mdns = false;
    config.tailscale = None;
    config.presence_interval = Duration::from_secs(3600);
    config.sessions = Some(store.clone());
    config
}

async fn peer(name: &str) -> Peer { peer_with(name, |_| {}).await }

async fn peer_with(name: &str, configure: impl FnOnce(&mut NodeConfig)) -> Peer {
    let dir = tempfile::tempdir().unwrap();
    let bus = Arc::new(Bus::new());
    let store = Store::new(dir.path().join("sessions"), JournalOptions::default()).unwrap();
    let mut config = config(dir.path(), name, &store);
    configure(&mut config);
    let identity = NodeIdentity::generate().unwrap();
    let node = Node::open(config, &identity, KeySource::Keychain, Arc::new(BusSessions::new(Arc::clone(&bus)))).unwrap();
    node.start().await.unwrap();
    Peer { node, bus, store, _dir: dir }
}

/// Two started nodes that know each other by their QUIC addresses.
async fn pair(owner: &str, guest: &str) -> (Peer, Peer) {
    let (owner, guest) = (peer(owner).await, peer(guest).await);
    meet(&owner.node, &guest.node).await;
    (owner, guest)
}

async fn meet(owner: &Node, guest: &Node) {
    guest.add_peer(&owner.key().to_string(), Some(&addresses(owner).0.to_string())).await.unwrap();
    owner.add_peer(&guest.key().to_string(), Some(&addresses(guest).0.to_string())).await.unwrap();
}

fn addresses(node: &Node) -> (SocketAddr, SocketAddr) {
    match node.status().endpoint {
        loa_p2p::Endpoint::Listening { udp, tcp } => (udp[0], tcp),
        other => panic!("not listening: {other:?}"),
    }
}

async fn grant(owner: &Node, guest: &Node, resource: Resource, action: Action) -> GrantId {
    owner.issue_grant(&guest.key().to_string(), vec![Scope::new(resource, [action]).unwrap()], None, false).await.unwrap()
}

/// Writes a session root, a turn and a user prompt; returns the turn.
fn seed(writer: &SessionWriter, id: &str, node: &Node, title: &str, prompt: &str) -> DomNodeId {
    let ts = now_ms();
    let attrs = [
        (attr::ID, Value::from(id)),
        (attr::HARNESS, Value::from("omp")),
        (attr::NODE, Value::from(node.key().to_string().as_str())),
        (attr::CREATED, Value::Int(ts)),
        (attr::SCHEMA, Value::Int(1)),
        (attr::TITLE, Value::from(title)),
        (attr::PROJECT, Value::from("/home/owner/secret-project")),
    ];
    let root = writer.commit_at(Draft::insert(DomNodeId::DOCUMENT, ElementKind::Session, attrs, ""), ts).unwrap();
    let turn = writer.commit_at(Draft::insert(root.node, ElementKind::Turn, std::iter::empty::<(&str, Value)>(), ""), ts).unwrap();
    writer.commit_at(Draft::insert(turn.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], prompt), ts).unwrap();
    turn.node
}

/// A live session on the owner's bus, with one prompt and an answer.
fn session(owner: &Peer, id: &str) -> (Arc<SessionWriter>, DomNodeId) {
    let key = loa_bus::NodeKey(*owner.node.key().as_bytes());
    let config = WriterConfig { session: SessionId::from(id), owner: key, local: key, dom: Dom::new(), persist: Box::new(Discard), authorize: owner.node.authorizer() };
    let writer = owner.bus.open(config).unwrap();
    let turn = seed(&writer, id, &owner.node, "Fix the parser", "Why does it fail?");
    let answer = writer.commit_at(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from("assistant"))], "Because"), now_ms()).unwrap();
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

fn first_message(dom: &Dom) -> DomNodeId { dom.iter().find(|n| n.kind == ElementKind::Message).unwrap().id }

fn given(node: &Node) -> Vec<GrantId> { node.grants().into_iter().filter(|g| g.direction == GrantDirection::Given && !g.ended).map(|g| g.id).collect() }

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopping_clears_peer_transports() {
    let (owner, guest) = pair("owner", "guest").await;
    assert_eq!(guest.node.peers()[0].transport, Some(TransportKind::Quic));
    guest.node.stop().await;
    assert_eq!(guest.node.peers()[0].transport, None);
    owner.node.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_shared_session_is_viewed_driven_prompted_forked_and_revoked() {
    let (owner, guest) = pair("owner-box", "guest-box").await;
    let seen = guest.node.peers();
    assert_eq!((seen.len(), seen[0].online, seen[0].transport), (1, true, Some(TransportKind::Quic)));
    assert_eq!((seen[0].app_version.as_deref(), seen[0].name.as_deref(), seen[0].shared_protocol), (Some("0.4.2"), Some("owner-box"), Some(true)));
    assert_eq!(seen[0].protocols, ["omp/control/1", "omp/session/1", "omp/folder/1"]);

    let (writer, answer) = session(&owner, "s-shared");
    let mut owner_events = owner.node.events();
    for action in [Action::Drive, Action::Fork] {
        grant(&owner.node, &guest.node, Resource::Session { id: "s-shared".into() }, action).await;
    }
    let received: Vec<_> = guest.node.grants().into_iter().filter(|g| g.direction == GrantDirection::Received).collect();
    assert_eq!(received.len(), 2, "both grants were delivered over omp/control/1");
    assert!(received.iter().all(|g| g.peer == owner.node.key() && g.renews_at_ms.is_some()));

    let owner_key = owner.node.key().to_string();
    let listed = guest.node.list_remote(&owner_key).await.unwrap();
    assert_eq!((listed.len(), listed[0].title.as_str()), (1, "Fix the parser"));
    assert!(listed[0].actions.contains(&Action::Drive) && listed[0].actions.contains(&Action::Fork));
    assert_eq!(guest.node.remote_sessions().len(), 1, "peer sessions are kept for Recents");

    let view = guest.node.open_remote(&owner_key, "s-shared").await.unwrap();
    assert!(view.can(Action::Drive) && view.can(Action::Fork));
    assert_eq!(view.source_seq(), writer.seq());
    view.writer().with_dom(|dom| {
        assert!(!dom.root().unwrap().attrs.contains_key(attr::PROJECT), "project paths stay with the owner");
        assert_eq!(text_of(dom, "assistant").as_deref(), Some("Because"));
    });

    writer.commit_at(Draft::append(answer, " the lexer drops tokens"), now_ms()).unwrap();
    eventually("the owner's patch on the view", || view.writer().with_dom(|dom| text_of(dom, "assistant").as_deref() == Some("Because the lexer drops tokens"))).await;

    // Drive goes through the owner, the single writer; a stale base is refused.
    let visible = view.writer().with_dom(|dom| dom.iter().find(|n| n.str_attr(attr::ROLE) == Some("assistant")).unwrap().id);
    let before = view.source_seq();
    assert_eq!(view.drive(Draft::append(visible, ".")).await.unwrap(), DriveOutcome::Applied { source_seq: before + 1 });
    writer.with_dom(|dom| assert_eq!(text_of(dom, "assistant").as_deref(), Some("Because the lexer drops tokens.")));
    eventually("the driven patch back on the view", || view.source_seq() == before + 1).await;
    writer.commit_at(Draft::append(answer, "!"), now_ms()).unwrap();
    assert_eq!(view.drive_at(before, Draft::append(visible, "?")).await.unwrap(), DriveOutcome::Conflict { current: before + 2 });

    view.prompt("Now write a test").await.unwrap();
    let requested = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(NodeEvent::PromptRequested { peer, session, text }) = owner_events.recv().await {
                break (peer, session, text);
            }
        }
    });
    assert_eq!(requested.await.unwrap(), (guest.node.key(), "s-shared".to_owned(), "Now write a test".to_owned()));

    let forked = guest.node.fork_remote(&owner_key, "s-shared").await.unwrap();
    let dom = guest.store.open(&forked).unwrap().0.load().unwrap();
    let root = dom.root().unwrap();
    assert_eq!((root.str_attr(attr::NODE), root.str_attr(attr::PARENT_SESSION)), (Some(guest.node.key().to_string().as_str()), Some("s-shared")));
    assert_eq!(text_of(&dom, "user").as_deref(), Some("Why does it fail?"));

    // Revoking stops the view at the owner's next patch and tells the guest.
    for id in given(&owner.node) {
        owner.node.revoke_grant(&id).await.unwrap();
    }
    writer.commit_at(Draft::append(answer, " after revoke"), now_ms()).unwrap();
    eventually("the view to end", || view.ended().is_some()).await;
    view.writer().with_dom(|dom| assert!(!text_of(dom, "assistant").unwrap().contains("after revoke")));
    assert!(guest.node.grants().iter().filter(|g| g.direction == GrantDirection::Received).all(|g| g.ended));
    assert!(guest.node.open_remote(&owner_key, "s-shared").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn view_only_grants_refuse_drive_and_hide_other_prompts() {
    let (owner, guest) = pair("owner", "guest").await;
    let (writer, _) = session(&owner, "s-view");
    let root = writer.with_dom(|dom| dom.root().unwrap().id);
    let second = writer.commit_at(Draft::insert(root, ElementKind::Turn, std::iter::empty::<(&str, Value)>(), ""), now_ms()).unwrap();
    writer.commit_at(Draft::insert(second.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], "only this one"), now_ms()).unwrap();

    grant(&owner.node, &guest.node, Resource::Prompt { session: "s-view".into(), turn: second.node }, Action::View).await;
    let owner_key = owner.node.key().to_string();
    let listed = guest.node.list_remote(&owner_key).await.unwrap();
    assert!(listed.len() == 1 && listed[0].partial && listed[0].actions.is_empty() && listed[0].title.is_empty());

    let view = guest.node.open_remote(&owner_key, "s-view").await.unwrap();
    let messages: Vec<_> = view.writer().with_dom(|dom| dom.iter().filter(|n| n.kind == ElementKind::Message).map(|n| n.text.clone()).collect());
    assert_eq!(messages, ["only this one"]);
    let message = view.writer().with_dom(first_message);
    assert!(view.drive(Draft::append(message, "!")).await.is_err(), "view-only grants can't drive");
    assert!(view.prompt("hi").await.is_err());
    assert!(guest.node.fork_remote(&owner_key, "s-view").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn grants_wait_for_offline_holders_renew_over_tcp_and_stop_after_revocation() {
    let owner = peer("owner").await;
    // A margin wider than the 10 min token life: every held token is due at once.
    let guest = peer_with("guest", |config| config.renew_margin_ms = 20 * 60_000).await;
    let guest_key = guest.node.key().to_string();
    owner.node.add_peer(&guest_key, Some(&addresses(&guest.node).0.to_string())).await.unwrap();
    assert!(owner.node.peers()[0].online);
    guest.node.stop().await;
    owner.node.refresh_presence().await;
    assert!(!owner.node.peers()[0].online, "a peer that stops answering is offline");

    let id = grant(&owner.node, &guest.node, Resource::Session { id: "s".into() }, Action::View).await;
    assert!(guest.node.grants().is_empty());
    guest.node.start().await.unwrap();
    // Only the owner's TCP address, before any QUIC contact: QUIC gets no answer, the fallback connects.
    guest.node.add_peer(&owner.node.key().to_string(), Some(&addresses(&owner.node).1.to_string())).await.unwrap();
    assert_eq!(guest.node.peers()[0].transport, Some(TransportKind::Tcp));
    // The guest restarted on new ports; the owner learns them like a user would.
    owner.node.remove_peer(&guest_key).unwrap();
    owner.node.add_peer(&guest_key, Some(&addresses(&guest.node).0.to_string())).await.unwrap();
    assert_eq!(guest.node.grants().iter().map(|g| &g.id).collect::<Vec<_>>(), [&id], "the waiting grant was delivered once the holder answered");
    let first = guest.node.grants()[0].expires_ms.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let outcomes = guest.node.renew_due().await;
    assert!(matches!(outcomes.as_slice(), [loa_trust::RenewOutcome::Renewed { .. }]), "{outcomes:?}");
    assert!(guest.node.grants()[0].expires_ms.unwrap() > first);

    owner.node.revoke_grant(&id).await.unwrap();
    assert!(guest.node.grants()[0].ended);
    assert!(guest.node.renew_due().await.is_empty(), "a refused grant stops renewing");
}

/// The app's indirection: the library's writers ask the node, which exists only after the library.
#[derive(Default)]
struct Late(std::sync::OnceLock<Arc<dyn loa_bus::Authorize>>);

impl loa_bus::Authorize for Late {
    fn authorize(&self, session: &SessionId, requester: &loa_bus::NodeKey, draft: &Draft, dom: &Dom) -> Result<(), loa_bus::Denied> {
        self.0.get().ok_or_else(|| loa_bus::Denied("no node yet".into()))?.authorize(session, requester, draft, dom)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn journaled_library_sessions_are_listed_viewed_and_driven_by_peers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path().join("sessions"), JournalOptions::default()).unwrap();
    let identity = NodeIdentity::generate().unwrap();
    let late = Arc::new(Late::default());
    let library = Arc::new(Library::new(store.clone(), Arc::new(Bus::new()), loa_bus::NodeKey(*identity.public_key().as_bytes()), Arc::clone(&late) as _));
    // The app's one grant store, outside the node's directory.
    let grants = Arc::new(GrantStore::open(&dir.path().join("grants.db")).unwrap());
    let sessions: Arc<dyn LocalSessions> = Arc::new(LibrarySessions::new(Arc::clone(&library)));
    let owner = Node::open_with_grants(config(dir.path(), "owner", &store), &identity, KeySource::Keychain, sessions, Arc::clone(&grants)).unwrap();
    assert!(late.0.set(owner.authorizer()).is_ok());
    owner.start().await.unwrap();

    // A session written, then closed: it lives only in its journal.
    seed(&library.create("s-journaled").unwrap().writer, "s-journaled", &owner, "Journaled", "hello");
    assert!(library.close("s-journaled").unwrap());
    assert!(owner.resources().iter().any(|(_, label)| label == "Journaled"), "closed sessions are grant resources");

    let guest = peer("guest").await;
    meet(&owner, &guest.node).await;
    grant(&owner, &guest.node, Resource::Session { id: "s-journaled".into() }, Action::Drive).await;
    assert_eq!(grants.issued().unwrap().len(), 1, "the node writes into the shared store");
    let listed = guest.node.list_remote(&owner.key().to_string()).await.unwrap();
    assert_eq!(listed.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["Journaled"]);

    let view = guest.node.open_remote(&owner.key().to_string(), "s-journaled").await.unwrap();
    let message = view.writer().with_dom(first_message);
    assert!(matches!(view.drive(Draft::append(message, " there")).await.unwrap(), DriveOutcome::Applied { .. }));
    // The owner journals the peer's change like its own.
    library.close("s-journaled").unwrap();
    assert_eq!(text_of(&store.open("s-journaled").unwrap().0.load().unwrap(), "user").as_deref(), Some("hello there"));
}

/// Echoes every byte back on the stream.
struct Echo;

#[async_trait::async_trait]
impl loa_p2p::PeerProtocol for Echo {
    async fn serve(&self, _peer: loa_p2p::NodeId, mut stream: loa_p2p::Stream) {
        let _ = tokio::io::copy(&mut stream.recv, &mut stream.send).await;
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream.send).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn added_protocols_serve_only_peers_with_a_covering_grant() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (owner, guest) = (peer("owner-box").await, peer("guest-box").await);
    let required = (Resource::Sandbox, Action::Exec);
    owner.node.add_protocol("loa-echo", 1, required.clone(), Arc::new(Echo));
    owner.node.stop().await;
    owner.node.start().await.unwrap();
    assert!(owner.node.resources().iter().any(|(resource, label)| *resource == Resource::Sandbox && label == loa_p2p::SANDBOX_LABEL));
    meet(&owner.node, &guest.node).await;

    let refused = guest.node.open_stream(owner.node.key(), "loa-echo", 1).await;
    assert!(matches!(refused, Err(NodeError::Refused(_))), "{:?}", refused.err());
    grant(&owner.node, &guest.node, Resource::Ssh { host: "box".into() }, Action::Exec).await;
    assert!(guest.node.open_stream(owner.node.key(), "loa-echo", 1).await.is_err(), "another resource doesn't cover it");

    let id = grant(&owner.node, &guest.node, Resource::Sandbox, Action::Exec).await;
    eventually("the sandbox grant at the guest", || guest.node.granting_peers(&required) == [owner.node.key()]).await;
    let mut stream = guest.node.open_stream(owner.node.key(), "loa-echo", 1).await.unwrap();
    stream.send.write_all(b"ping").await.unwrap();
    stream.send.shutdown().await.unwrap();
    let mut echoed = Vec::new();
    stream.recv.read_to_end(&mut echoed).await.unwrap();
    assert_eq!(echoed, b"ping");

    owner.node.revoke_grant(&id).await.unwrap();
    assert!(guest.node.open_stream(owner.node.key(), "loa-echo", 1).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn folder_grants_list_read_and_write_only_inside_the_folder() {
    let (owner, guest) = pair("owner-box", "guest-box").await;
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
    let (owner_key, g) = (owner.node.key().to_string(), &guest.node);
    assert!(matches!(g.list_folder(&owner_key, &folder, "").await, Err(NodeError::Refused(_))));

    // A read grant: list, read (several frames), no writes, nothing outside.
    let id = grant(&owner.node, g, Resource::Folder { path: folder.clone() }, Action::Read).await;
    eventually("the folder grant at the guest", || !g.shared_folders().is_empty()).await;
    assert_eq!(g.shared_folders(), [loa_p2p::SharedFolder { peer: owner.node.key(), path: folder.clone(), can_write: false }]);
    let entries = g.list_folder(&owner_key, &folder, "").await.unwrap();
    assert_eq!(entries.iter().map(|e| (e.name.as_str(), e.dir)).collect::<Vec<_>>(), [("docs", true), ("notes.txt", false)]);
    assert_eq!(g.read_folder_file(&owner_key, &folder, "docs/big.bin").await.unwrap(), big);
    assert_eq!(g.list_folder(&owner_key, &format!("{folder}/docs"), "").await.unwrap()[0].size, big.len() as u64);
    for escape in ["../secret.txt", "/etc/hostname", "docs/../../secret.txt"] {
        assert!(g.read_folder_file(&owner_key, &folder, escape).await.is_err(), "{escape}");
    }
    assert!(g.list_folder(&owner_key, base.to_str().unwrap(), "").await.is_err(), "the folder above");
    let write = g.write_folder_file(&owner_key, &folder, "notes.txt", b"changed").await;
    assert!(matches!(&write, Err(NodeError::Refused(error)) if error.contains("no grant")), "{write:?}");

    // A write grant writes, in several frames too.
    grant(&owner.node, g, Resource::Folder { path: folder.clone() }, Action::Write).await;
    eventually("the write grant at the guest", || g.shared_folders().first().is_some_and(|f| f.can_write)).await;
    g.write_folder_file(&owner_key, &folder, "docs/copy.bin", &big).await.unwrap();
    assert_eq!(std::fs::read(share.join("docs/copy.bin")).unwrap(), big);
    g.write_folder_file(&owner_key, &folder, "empty.txt", b"").await.unwrap();
    assert_eq!(std::fs::read(share.join("empty.txt")).unwrap(), b"");
    g.write_folder_file(&owner_key, &folder, "notes.txt", b"updated").await.unwrap();
    assert_eq!(std::fs::read(share.join("notes.txt")).unwrap(), b"updated");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("notes.txt", share.join("link.txt")).unwrap();
        assert!(g.write_folder_file(&owner_key, &folder, "link.txt", b"changed").await.is_err());
        assert_eq!(std::fs::read(share.join("notes.txt")).unwrap(), b"updated");
        assert!(std::fs::symlink_metadata(share.join("link.txt")).unwrap().file_type().is_symlink());
    }
    assert!(g.write_folder_file(&owner_key, &folder, "../escaped.txt", b"x").await.is_err());
    assert!(!base.join("escaped.txt").exists());
    assert!(owner.node.resources().iter().any(|(resource, label)| *resource == Resource::Folder { path: folder.clone() } && *label == format!("Folder {folder}")));

    // Revoking the read grant leaves the write grant working; revoking it too ends access.
    owner.node.revoke_grant(&id).await.unwrap();
    assert!(g.list_folder(&owner_key, &folder, "").await.is_ok());
    for id in given(&owner.node) {
        owner.node.revoke_grant(&id).await.unwrap();
    }
    assert!(g.list_folder(&owner_key, &folder, "").await.is_err());
}
