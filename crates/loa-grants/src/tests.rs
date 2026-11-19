use super::*;
use futures::executor::block_on;
use loa_core::{Dom, ElementKind, Node, NodeId, Value, attr};
use std::collections::BTreeMap;

const NOW: i64 = 1_800_000_000_000;
const TTL: i64 = 60_000;

fn key(byte: u8) -> NodeKey { NodeKey::from_secret(&[byte; 32]).unwrap() }
fn scope(resource: Resource, action: Action) -> Scope { Scope::new(resource, [action]).unwrap() }
fn session(id: &str, project: Option<&str>, created_ms: i64) -> SessionFacts {
    SessionFacts { id: id.into(), project: project.map(str::to_owned), created_ms }
}
fn verify(root: &NodeKey, store: &GrantStore, holder: &NodeKey, token: &[u8]) -> VerifiedGrant {
    let challenge = Challenge([19; 32]);
    let proof = prove(holder, &challenge, token);
    Verifier::new(root.public(), store).verify(Presented { token, proof: &proof }, &holder.public(), &challenge, NOW).unwrap()
}
fn issue(root: &NodeKey, store: &GrantStore, holder: &NodeKey, scopes: Vec<Scope>, reshare: bool) -> IssuedGrant {
    store.issue(root, holder.public(), GrantSpec { scopes, ttl_ms: TTL, reshare }, NOW).unwrap()
}

#[test]
fn token_requires_holder_private_key_and_fresh_challenge() {
    let root = key(1); let alice = key(2); let bob = key(3);
    let store = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let issued = issue(&root, &store, &alice, vec![scope(Resource::Session { id: "s".into() }, Action::View)], false);
    let nonce = Challenge([17; 32]);
    let stolen_proof = prove(&bob, &nonce, &issued.token);
    let verifier = Verifier::new(root.public(), &store);
    let copied = Presented { token: &issued.token, proof: &stolen_proof };
    assert!(matches!(verifier.verify(copied, &bob.public(), &nonce, NOW), Err(Denied::NotHolder { .. })));
    assert!(matches!(verifier.verify(copied, &alice.public(), &nonce, NOW), Err(Denied::BadProof)));
    let proof = prove(&alice, &nonce, &issued.token);
    assert!(matches!(verifier.verify(Presented { token: &issued.token, proof: &proof }, &alice.public(), &Challenge([18; 32]), NOW), Err(Denied::BadProof)));
    assert!(matches!(verifier.verify(Presented { token: &issued.token, proof: &proof }, &alice.public(), &nonce, NOW + TTL), Err(Denied::Expired { .. })));
}

#[test]
fn typed_scopes_authorize_resources_without_leaking_others() {
    let root = key(4); let alice = key(5);
    let store = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let issued = issue(&root, &store, &alice, vec![
        scope(Resource::Session { id: "one".into() }, Action::Suggest),
        scope(Resource::Prompt { session: "two".into(), turn: NodeId(3) }, Action::View),
        scope(Resource::Project { id: "work".into() }, Action::Fork),
        scope(Resource::FutureSessions { after_ms: NOW }, Action::Drive),
        scope(Resource::Ssh { host: "allowed".into() }, Action::Read),
        scope(Resource::Surface { uri: "surface://browser/tab".into() }, Action::View),
    ], false);
    let grant = verify(&root, &store, &alice, &issued.token);
    let before = session("two", None, NOW - 1);
    let project = session("three", Some("work"), NOW - 1);
    let future = session("new", None, NOW);
    assert!(grant.authorize(&Request::session(Action::Suggest, &session("one", None, NOW - 1)), NOW).is_ok());
    assert!(matches!(grant.authorize(&Request::session(Action::Drive, &session("one", None, NOW - 1)), NOW), Err(Denied::NotGranted { .. })));
    assert!(grant.authorize(&Request::turn(Action::View, &before, NodeId(3)), NOW).is_ok());
    assert!(grant.authorize(&Request::turn(Action::View, &before, NodeId(4)), NOW).is_err());
    assert!(grant.authorize(&Request::session(Action::View, &before), NOW).is_err());
    assert!(grant.authorize(&Request::session(Action::Fork, &project), NOW).is_ok());
    assert!(grant.authorize(&Request::session(Action::View, &future), NOW).is_ok());
    assert!(grant.authorize(&Request::session(Action::View, &session("old", None, NOW - 1)), NOW).is_err());
    assert!(grant.authorize(&Request::ssh(Action::Read, "allowed"), NOW).is_ok());
    assert!(grant.authorize(&Request::ssh(Action::Exec, "allowed"), NOW).is_err());
    assert!(grant.authorize(&Request::ssh(Action::Read, "other"), NOW).is_err());
    assert!(grant.authorize(&Request::surface(Action::View, "surface://browser/tab"), NOW).is_ok());
    assert!(grant.authorize(&Request::surface(Action::Drive, "surface://browser/tab"), NOW).is_err());
    assert!(grant.authorize(&Request::surface(Action::View, "surface://browser/other"), NOW).is_err());
    let available = [session("old", None, NOW - 1), future];
    assert_eq!(grant.visible_sessions(available.iter(), NOW).len(), 1);
}

#[test]
fn ssh_tool_call_checks_host_and_operation_not_just_tool_listing() {
    let root = key(20);
    let alice = key(21);
    let store = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let issued = issue(&root, &store, &alice, vec![
        scope(Resource::Session { id: "s".into() }, Action::Drive),
        scope(Resource::Ssh { host: "read-only".into() }, Action::Read),
    ], false);
    let grant = verify(&root, &store, &alice, &issued.token);
    let facts = session("s", None, NOW);
    let hosts = ["read-only".to_owned(), "private".to_owned()];
    let access = ToolAccess::new(&grant, &facts, &hosts, &[], NOW);
    assert!(access.allows(&Tool { name: "remote_file".into(), source: ToolSource::Mcp { server: REMOTE_SERVER.into() } }));
    assert!(access.allows_ssh_call("read-only", Action::Read));
    assert!(!access.allows_ssh_call("read-only", Action::Write));
    assert!(!access.allows_ssh_call("read-only", Action::Exec));
    assert!(!access.allows_ssh_call("private", Action::Read));
}

#[test]
fn reshare_attenuates_scopes_and_expiry_across_holders() {
    let root = key(6); let alice = key(7); let bob = key(8); let carol = key(9);
    let issuer = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let alice_store = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let bob_store = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let parent = issue(&root, &issuer, &alice, vec![scope(Resource::Session { id: "one".into() }, Action::Drive), scope(Resource::Session { id: "two".into() }, Action::View)], true);
    alice_store.receive(&alice.public(), &root.public(), &parent.token).unwrap();
    let child = alice_store.reshare(&alice, &parent.id, bob.public(), Narrowing {
        scopes: Some(vec![scope(Resource::Session { id: "one".into() }, Action::View)]),
        ttl_ms: 2 * TTL, reshare: true,
    }, NOW).unwrap();
    assert_eq!(child.expires_ms, parent.expires_ms);
    bob_store.receive(&bob.public(), &root.public(), &child.token).unwrap();
    let grandchild = bob_store.reshare(&bob, &child.id, carol.public(), Narrowing {
        scopes: Some(vec![scope(Resource::Session { id: "two".into() }, Action::View)]),
        ttl_ms: TTL, reshare: false,
    }, NOW).unwrap();
    let grant = verify(&root, &issuer, &bob, &child.token);
    assert_eq!(grant.chain().len(), 2);
    assert!(grant.authorize(&Request::session(Action::View, &session("one", None, NOW)), NOW).is_ok());
    assert!(matches!(grant.authorize(&Request::session(Action::Drive, &session("one", None, NOW)), NOW), Err(Denied::Narrowed { .. })));
    assert!(matches!(grant.authorize(&Request::session(Action::View, &session("two", None, NOW)), NOW), Err(Denied::Narrowed { .. })));
    let last = verify(&root, &issuer, &carol, &grandchild.token);
    assert!(last.authorize(&Request::session(Action::View, &session("two", None, NOW)), NOW).is_err());
    assert!(last.authorize(&Request::session(Action::View, &session("one", None, NOW)), NOW).is_err());
    assert!(matches!(last.authorize(&Request::session(Action::View, &session("one", None, NOW)), NOW + TTL), Err(Denied::Expired { .. })));
}

#[test]
fn revoked_grant_refuses_renewal_and_disallowed_reshare() {
    let root = key(10); let alice = key(11); let bob = key(12);
    let issuer = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let alice_store = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let issued = issue(&root, &issuer, &alice, vec![scope(Resource::Session { id: "one".into() }, Action::View)], false);
    alice_store.receive(&alice.public(), &root.public(), &issued.token).unwrap();
    assert!(matches!(alice_store.reshare(&alice, &issued.id, bob.public(), Narrowing { scopes: None, ttl_ms: TTL, reshare: false }, NOW), Err(GrantError::ReshareForbidden(_))));
    assert!(issuer.renew(&root, &issued.id, &bob.public(), NOW).is_err());
    assert!(issuer.renew(&root, &issued.id, &alice.public(), NOW + 10).is_ok());
    issuer.revoke(&root.public(), &issued.id, NOW + 20).unwrap();
    assert!(matches!(issuer.renew(&root, &issued.id, &alice.public(), NOW + 21), Err(GrantError::Revoked(_))));
    let nonce = Challenge([1; 32]); let proof = prove(&alice, &nonce, &issued.token);
    assert!(matches!(Verifier::new(root.public(), &issuer).verify(Presented { token: &issued.token, proof: &proof }, &alice.public(), &nonce, NOW + 21), Err(Denied::Revoked(_))));
}

#[test]
fn prompt_visibility_selects_only_its_turn_and_future_excludes_old_digests() {
    let root = key(13); let alice = key(14);
    let store = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let issued = issue(&root, &store, &alice, vec![scope(Resource::Prompt { session: "s".into(), turn: NodeId(2) }, Action::View)], false);
    let grant = verify(&root, &store, &alice, &issued.token);
    let mut attrs = BTreeMap::new();
    attrs.insert(attr::ID.into(), Value::Str("s".into()));
    attrs.insert(attr::CREATED.into(), Value::Int(NOW));
    let dom = Dom::from_parts([
        Node { id: NodeId(1), kind: ElementKind::Session, parent: NodeId::DOCUMENT, children: vec![NodeId(2), NodeId(4)], attrs, text: String::new() },
        Node { id: NodeId(2), kind: ElementKind::Turn, parent: NodeId(1), children: vec![NodeId(3)], attrs: BTreeMap::new(), text: String::new() },
        Node { id: NodeId(3), kind: ElementKind::Message, parent: NodeId(2), children: vec![], attrs: BTreeMap::new(), text: "private prompt".into() },
        Node { id: NodeId(4), kind: ElementKind::Turn, parent: NodeId(1), children: vec![], attrs: BTreeMap::new(), text: String::new() },
    ], NodeId(5), 0).unwrap();
    let visible = grant.visibility(&dom, Action::View, NOW);
    assert_eq!(visible, Visibility::Turns(vec![NodeId(2)]));
    assert!(visible.allows(&dom, NodeId(3)));
    assert!(!visible.allows(&dom, NodeId(1)));
    assert!(!visible.allows(&dom, NodeId(4)));
}

struct LocalTransport<'a> { issuer: &'a GrantStore, key: &'a NodeKey, holder: NodePublicKey }
impl RenewTransport for LocalTransport<'_> {
    async fn renew(&self, _: NodePublicKey, id: GrantId) -> std::result::Result<Vec<u8>, RenewFailure> {
        self.issuer.renew(self.key, &id, &self.holder, NOW + 40_000).map(|g| g.token).map_err(|e| RenewFailure::from_issuer(&e))
    }
}
#[test]
fn renewal_driver_refreshes_and_stops_after_refusal() {
    let root = key(15); let alice = key(16);
    let issuer = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let holder = GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
    let issued = issue(&root, &issuer, &alice, vec![scope(Resource::Session { id: "s".into() }, Action::View)], false);
    holder.receive(&alice.public(), &root.public(), &issued.token).unwrap();
    let driver = Renewer::new(alice.public(), 30_000);
    let transport = LocalTransport { issuer: &issuer, key: &root, holder: alice.public() };
    assert_eq!(driver.next_due(&holder).unwrap(), Some(NOW + 30_000));
    assert!(block_on(driver.renew_due(&holder, &transport, NOW + 20_000)).unwrap().is_empty());
    assert!(matches!(block_on(driver.renew_due(&holder, &transport, NOW + 40_000)).unwrap().as_slice(), [RenewOutcome::Renewed { .. }]));
    issuer.revoke(&root.public(), &issued.id, NOW + 41_000).unwrap();
    assert!(matches!(block_on(driver.renew_due(&holder, &transport, NOW + 80_000)).unwrap().as_slice(), [RenewOutcome::Refused { .. }]));
    assert_eq!(driver.next_due(&holder).unwrap(), None);
}
