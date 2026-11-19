//! Round trip through the real OS keychain. Ignored by default: it needs a
//! running credential service and writes (then deletes) one entry of its own.

use loa_identity::{KeyStore, NodeIdentity, OsKeychain};

#[test]
#[ignore = "touches the real OS keychain"]
fn node_key_round_trips_through_os_keychain() {
    let account = format!(
        "test-{}-{}",
        std::process::id(),
        NodeIdentity::generate().unwrap().public_key()
    );
    let keychain = OsKeychain::new("loa-identity-test", &account).unwrap();
    assert!(keychain.load().unwrap().is_none());

    let created = NodeIdentity::load_or_create(&keychain);
    let reopened = OsKeychain::new("loa-identity-test", &account).unwrap();
    let loaded = NodeIdentity::load_or_create(&reopened);
    let deleted = keychain.delete();

    let created = created.unwrap();
    assert_eq!(created.public_key(), loaded.unwrap().public_key());
    deleted.unwrap();
    assert!(keychain.load().unwrap().is_none());
}
