use super::*;
use futures::executor::block_on;
use loa_core::NodeId;

const NOW: i64 = 1_800_000_000_000;
const TTL: i64 = 60_000;

fn key(byte: u8) -> NodeIdentity {
    NodeIdentity::from_secret_bytes(&[byte; 32])
}
fn scope(resource: Resource, action: Action) -> Scope {
    Scope::new(resource, [action]).unwrap()
}
fn store() -> GrantStore {
    GrantStore::with_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap()
}
fn session(id: &str, project: Option<&str>, created_ms: i64) -> SessionFacts {
    SessionFacts { id: id.into(), project: project.map(str::to_owned), created_ms }
}
fn verify(root: &NodeIdentity, store: &GrantStore, holder: &NodeIdentity, token: &[u8], now: i64) -> Result<VerifiedGrant, Denied> {
    let challenge = Challenge([19; 32]);
    let proof = prove(holder, &challenge, token);
    Verifier::new(root.public_key(), store).verify(Presented { token, proof: &proof }, &holder.public_key(), &challenge, now)
}
fn issue(root: &NodeIdentity, store: &GrantStore, holder: &NodeIdentity, scopes: Vec<Scope>, reshare: bool) -> IssuedGrant {
    store.issue(root, holder.public_key(), GrantSpec { scopes, ttl_ms: TTL, reshare, until_ms: None }, NOW).unwrap()
}

#[test]
fn authorize_matrix_covers_every_kind_and_prefix_traps() {
    let (root, alice) = (key(1), key(2));
    let store = store();
    let given = vec![
        scope(Resource::Session { id: "one".into() }, Action::Suggest),
        scope(Resource::Prompt { session: "two".into(), turn: NodeId(3) }, Action::View),
        scope(Resource::Project { id: "work".into() }, Action::Fork),
        scope(Resource::FutureSessions { after_ms: NOW }, Action::Drive),
        scope(Resource::Ssh { host: "box".into() }, Action::Read),
        scope(Resource::Surface { uri: "surface://browser/tab".into() }, Action::View),
        scope(Resource::Sandbox, Action::Exec),
        scope(Resource::Folder { path: "/srv/share".into() }, Action::Write),
    ];
    let issued = issue(&root, &store, &alice, given.clone(), false);
    let grant = verify(&root, &store, &alice, &issued.token, NOW).unwrap();
    let (one, two, onex, work, new, old) =
        (session("one", None, 0), session("two", None, 0), session("one-more", None, 0), session("w", Some("work"), 0), session("n", None, NOW), session("o", Some("workshop"), NOW - 1));
    use Action::*;
    let rows: Vec<(Request, bool)> = vec![
        (Request::session(Suggest, &one), true),
        (Request::session(View, &one), true),
        (Request::session(Drive, &one), false),
        (Request::session(View, &onex), false),
        (Request::turn(View, &two, NodeId(3)), true),
        (Request::turn(View, &two, NodeId(4)), false),
        (Request::session(View, &two), false),
        (Request::session(Fork, &work), true),
        (Request::session(Drive, &work), false),
        (Request::session(Drive, &new), true),
        (Request::session(View, &old), false),
        (Request::ssh(Read, "box"), true),
        (Request::ssh(Exec, "box"), false),
        (Request::ssh(Read, "box2"), false),
        (Request::surface(View, "surface://browser/tab"), true),
        (Request::surface(Drive, "surface://browser/tab"), false),
        (Request::surface(View, "surface://browser/tab2"), false),
        (Request::sandbox(Exec), true),
        (Request::folder(Write, "/srv/share/docs/deep"), true),
        (Request::folder(Read, "/srv/share"), true),
        (Request::folder(Read, "/srv/shared"), false),
        (Request::folder(Read, "/srv"), false),
        (Request::folder(Read, "srv/share"), false),
        (Request::folder(Read, "/srv/share/../etc"), false),
        (Request::ssh(Read, "/srv/share"), false),
    ];
    for (request, allowed) in rows {
        let result = grant.authorize(&request, NOW);
        assert_eq!(result.is_ok(), allowed, "{request:?}: {result:?}");
        if !allowed {
            assert!(matches!(result, Err(Denied::NotGranted { .. })), "{request:?}: {result:?}");
        }
    }
    assert!(matches!(grant.authorize(&Request::sandbox(Exec), NOW + TTL), Err(Denied::Expired { .. })));
    // Scopes read back from the token, implied actions included.
    let mut read = token_scopes(&issued.token, &root.public_key()).unwrap();
    read.sort_by_key(|s| format!("{:?}", s.resource));
    let mut expected = given;
    expected.sort_by_key(|s| format!("{:?}", s.resource));
    assert_eq!(read, expected);
    // Actions must fit the kind; folders need normalized absolute paths.
    assert!(matches!(Scope::new(Resource::Ssh { host: "b".into() }, [View]), Err(TrustError::Action { action: View, .. })));
    assert!(Scope::new(Resource::Sandbox, [View]).is_err());
    assert!(matches!(Scope::new(Resource::Project { id: "p".into() }, []), Err(TrustError::NoActions)));
    assert_eq!(Scope::new(Resource::Surface { uri: "s".into() }, [Drive]).unwrap().actions, [View, Drive].into());
    for bad in ["relative", "/a/../b", "/a/", "/a//b", ""] {
        assert!(matches!(Scope::new(Resource::Folder { path: bad.into() }, [Read]), Err(TrustError::FolderPath(_))), "{bad:?}");
    }
}

#[test]
fn reshare_chain_narrows_needs_the_holder_key_and_stops_at_revocation() {
    let (root, alice, bob, carol) = (key(6), key(7), key(8), key(9));
    let (issuer, alice_store, bob_store) = (store(), store(), store());
    let folder = |path: &str| Resource::Folder { path: path.into() };
    let one = Resource::Session { id: "one".into() };
    let parent = issue(&root, &issuer, &alice, vec![scope(one.clone(), Action::Drive), scope(folder("/srv"), Action::Write)], true);
    alice_store.receive(&alice.public_key(), &root.public_key(), &parent.token).unwrap();
    let narrowing = Narrowing { scopes: Some(vec![scope(one.clone(), Action::View), scope(folder("/srv/docs"), Action::Read)]), ttl_ms: 2 * TTL, reshare: true };
    let child = alice_store.reshare(&alice, &parent.id, bob.public_key(), narrowing, NOW).unwrap();
    assert_eq!(child.expires_ms, parent.expires_ms);

    // Only the last holder, signing this challenge, can use the token.
    let nonce = Challenge([17; 32]);
    let stolen = prove(&carol, &nonce, &child.token);
    let verifier = Verifier::new(root.public_key(), &issuer);
    assert!(matches!(verifier.verify(Presented { token: &child.token, proof: &stolen }, &carol.public_key(), &nonce, NOW), Err(Denied::NotHolder { .. })));
    assert!(matches!(verifier.verify(Presented { token: &child.token, proof: &stolen }, &bob.public_key(), &nonce, NOW), Err(Denied::BadProof)));

    let grant = verify(&root, &issuer, &bob, &child.token, NOW).unwrap();
    assert_eq!(grant.chain().len(), 2);
    let s1 = session("one", None, NOW);
    assert!(grant.authorize(&Request::session(Action::View, &s1), NOW).is_ok());
    assert!(grant.authorize(&Request::folder(Action::Read, "/srv/docs/x"), NOW).is_ok());
    for denied in [Request::session(Action::Drive, &s1), Request::folder(Action::Read, "/srv"), Request::folder(Action::Write, "/srv/docs")] {
        assert!(matches!(grant.authorize(&denied, NOW), Err(Denied::Narrowed { block: 1, .. })), "{denied:?}");
    }

    bob_store.receive(&bob.public_key(), &root.public_key(), &child.token).unwrap();
    let last = Narrowing { scopes: Some(vec![scope(folder("/srv/docs"), Action::Read)]), ttl_ms: TTL, reshare: false };
    let grandchild = bob_store.reshare(&bob, &child.id, carol.public_key(), last, NOW).unwrap();
    let carols = verify(&root, &issuer, &carol, &grandchild.token, NOW).unwrap();
    assert!(carols.authorize(&Request::folder(Action::Read, "/srv/docs"), NOW).is_ok());
    assert!(carols.authorize(&Request::session(Action::View, &s1), NOW).is_err());
    let carol_store = store();
    carol_store.receive(&carol.public_key(), &root.public_key(), &grandchild.token).unwrap();
    let again = Narrowing { scopes: None, ttl_ms: TTL, reshare: false };
    assert!(matches!(carol_store.reshare(&carol, &grandchild.id, alice.public_key(), again, NOW), Err(TrustError::ReshareForbidden(_))));

    // Revoking the root grant denies every token cut from it.
    issuer.revoke(&root.public_key(), &parent.id, NOW + 1).unwrap();
    assert!(matches!(verify(&root, &issuer, &carol, &grandchild.token, NOW + 2), Err(Denied::Revoked(_))));
    assert!(matches!(issuer.renew(&root, &parent.id, &alice.public_key(), NOW + 2), Err(TrustError::Revoked(_))));
}

struct LocalTransport<'a> {
    issuer: &'a GrantStore,
    key: &'a NodeIdentity,
    holder: PublicKey,
}
impl RenewTransport for LocalTransport<'_> {
    async fn renew(&self, _: PublicKey, id: GrantId) -> std::result::Result<Vec<u8>, RenewFailure> {
        self.issuer.renew(self.key, &id, &self.holder, NOW + 40_000).map(|g| g.token).map_err(|e| RenewFailure::from_issuer(&e))
    }
}

#[test]
fn renewal_driver_refreshes_honours_the_end_and_stops_after_refusal() {
    let (root, alice) = (key(15), key(16));
    let (issuer, holder) = (store(), store());
    let issued = issue(&root, &issuer, &alice, vec![scope(Resource::Session { id: "s".into() }, Action::View)], false);
    holder.receive(&alice.public_key(), &root.public_key(), &issued.token).unwrap();
    assert!(issuer.renew(&root, &issued.id, &key(99).public_key(), NOW).is_err());
    let driver = Renewer::new(alice.public_key(), 30_000);
    let transport = LocalTransport { issuer: &issuer, key: &root, holder: alice.public_key() };
    assert_eq!(driver.next_due(&holder).unwrap(), Some(NOW + 30_000));
    assert!(block_on(driver.renew_due(&holder, &transport, NOW + 20_000)).unwrap().is_empty());
    assert!(matches!(block_on(driver.renew_due(&holder, &transport, NOW + 40_000)).unwrap().as_slice(), [RenewOutcome::Renewed { .. }]));

    // An end caps tokens and stops renewal; extending restarts it.
    let until = NOW + TTL / 2;
    let extended = issuer.extend(&root, &issued.id, Some(until), NOW).unwrap();
    assert_eq!(extended.expires_ms, until);
    assert!(matches!(issuer.renew(&root, &issued.id, &alice.public_key(), until), Err(TrustError::Expired(_))));
    issuer.extend(&root, &issued.id, None, until).unwrap();
    let records = issuer.issued().unwrap();
    assert_eq!((records.len(), records[0].holder, records[0].until_ms), (1, alice.public_key(), None));

    issuer.revoke(&root.public_key(), &issued.id, NOW + 41_000).unwrap();
    assert!(issuer.issued().unwrap()[0].revoked_ms.is_some());
    assert!(matches!(block_on(driver.renew_due(&holder, &transport, NOW + 80_000)).unwrap().as_slice(), [RenewOutcome::Refused { .. }]));
    assert_eq!(driver.next_due(&holder).unwrap(), None);
}

#[test]
fn keystore_fallback_migrates_and_never_switches_on_bad_values() {
    struct NoKeychain;
    impl KeyStore for NoKeychain {
        fn load(&self) -> Result<Option<SecretBytes>> {
            Err(TrustError::Unavailable("no secret service".into()))
        }
        fn store(&self, _: &[u8]) -> Result<()> {
            Err(TrustError::Unavailable("no secret service".into()))
        }
        fn delete(&self) -> Result<()> {
            Ok(())
        }
    }
    let dir = std::env::temp_dir().join(format!("loa-trust-keys-{}", std::process::id()));
    let fallback = FallbackStore::new(NoKeychain, FileKeyStore::in_dir(&dir));
    let first = NodeIdentity::load_or_create(&fallback).unwrap();
    assert!(matches!(fallback.source(), KeySource::File { keychain_error, path } if keychain_error.contains("secret service") && path.ends_with("node.key")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.join("node.key")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // Once a keychain works, the file key moves into it and the address stays.
    let migrated = FallbackStore::new(MemoryKeyStore::new(), FileKeyStore::in_dir(&dir));
    assert_eq!(NodeIdentity::load_or_create(&migrated).unwrap().public_key(), first.public_key());
    assert_eq!(migrated.source(), KeySource::Keychain);
    assert!(FileKeyStore::in_dir(&dir).load().unwrap().is_none());
    // A malformed keychain value is an error, not a reason to switch stores.
    let bad = FallbackStore::new(MemoryKeyStore::with_secret(&[1, 2, 3]), FileKeyStore::in_dir(&dir));
    assert!(matches!(NodeIdentity::load_or_create(&bad), Err(TrustError::WrongLength { found: 3 })));
    assert!(FileKeyStore::in_dir(&dir).load().unwrap().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn one_key_for_iroh_biscuit_and_the_bus() {
    let id = key(42);
    let iroh = iroh_base::SecretKey::from_bytes(id.secret_bytes()).public();
    assert_eq!(PublicKey::from(iroh), id.public_key());
    assert_eq!(iroh_base::PublicKey::try_from(id.public_key()).unwrap(), iroh);
    assert_eq!(iroh.to_string(), id.public_key().to_string());
    assert_eq!(iroh.to_string().parse::<PublicKey>().unwrap(), id.public_key());
    assert_eq!(PublicKey::from(loa_bus::NodeKey::from(id.public_key())), id.public_key());
    let signature = id.sign(b"dial me");
    iroh.verify(b"dial me", &iroh_base::Signature::from_bytes(&signature)).unwrap();
    assert!(id.public_key().verify(b"dial me", &signature) && !id.public_key().verify(b"dial you", &signature));
    assert!("not a key".parse::<PublicKey>().is_err());
}

/// A token minted and re-shared by the code before the [`KINDS`] table
/// (root seed 1, holder seed 2, re-shared to seed 3, valid until 5.8e12).
#[test]
fn tokens_minted_before_the_kind_table_still_verify() {
    let token = include_bytes!("../tests/fixtures/v1_reshared_token.bin");
    let (root, bob) = (key(1), key(3));
    let store = store();
    let grant = verify(&root, &store, &bob, token, NOW).unwrap();
    assert_eq!(grant.chain().len(), 2);
    let one = session("one", None, 0);
    assert!(grant.authorize(&Request::session(Action::View, &one), NOW).is_ok());
    assert!(grant.authorize(&Request::folder(Action::Read, "/srv/share/docs/a"), NOW).is_ok());
    assert!(matches!(grant.authorize(&Request::session(Action::Drive, &one), NOW), Err(Denied::Narrowed { .. })));
    assert!(matches!(grant.authorize(&Request::sandbox(Action::Exec), NOW), Err(Denied::Narrowed { .. })));
    let scopes = token_scopes(token, &root.public_key()).unwrap();
    assert_eq!(scopes.len(), KINDS.len());
    assert!(scopes.contains(&Scope::new(Resource::Prompt { session: "two".into(), turn: NodeId(3) }, [Action::View]).unwrap()));
    assert!(scopes.contains(&Scope::new(Resource::FutureSessions { after_ms: NOW }, [Action::Suggest]).unwrap()));
}
