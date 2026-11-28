//! Golden files for the C++ port, written by `cargo test -p loa-trust --lib
//! write_golden -- --ignored` into `$LOA_GOLDEN_OUT`. A child of `store` so the
//! schema text and the issued-row insert are reachable.

use super::*;
use crate::keystore::{FileKeyStore, KeyStore};
use crate::scope::{Action, Resource};
use rusqlite::types::ValueRef;
use std::fs;
use std::path::PathBuf;

const NOW: i64 = 1_800_000_000_000;

fn out(sub: &str) -> PathBuf {
    let dir = PathBuf::from(std::env::var("LOA_GOLDEN_OUT").expect("LOA_GOLDEN_OUT")).join(sub);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Every row of `tables` as JSON: one object per row, columns in table order,
/// blobs as `{"hex": ...}`.
fn dump(db: &Connection, tables: &[&str]) -> String {
    let mut all = serde_json::Map::new();
    for table in tables {
        let mut statement = db.prepare(&format!("SELECT * FROM {table} ORDER BY rowid")).unwrap();
        let names: Vec<String> = statement.column_names().into_iter().map(str::to_owned).collect();
        let rows: Vec<serde_json::Value> = statement
            .query_map([], |r| {
                let mut row = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    let value = match r.get_ref(i)? {
                        ValueRef::Null => serde_json::Value::Null,
                        ValueRef::Integer(n) => n.into(),
                        ValueRef::Real(f) => f.into(),
                        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned().into(),
                        ValueRef::Blob(b) => serde_json::json!({ "hex": data_encoding::HEXLOWER.encode(b) }),
                    };
                    row.insert(name.clone(), value);
                }
                Ok(serde_json::Value::Object(row))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        all.insert((*table).to_owned(), rows.into());
    }
    serde_json::to_string_pretty(&all).unwrap() + "\n"
}

#[test]
#[ignore = "writes golden files; run with LOA_GOLDEN_OUT set"]
fn write_golden_trust_and_grants() {
    // node.key: the raw seed FileKeyStore writes, owner-only.
    let trust = out("trust");
    let identity = NodeIdentity::from_secret_bytes(&[7; 32]);
    let _ = fs::remove_file(trust.join("node.key"));
    FileKeyStore::in_dir(&trust).store(identity.secret_bytes()).unwrap();
    let written = fs::read(trust.join("node.key")).unwrap();
    assert_eq!(written.len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(trust.join("node.key")).unwrap().permissions().mode() & 0o777, 0o600);
    }
    fs::write(trust.join("node.pub"), format!("{}\n", identity.public_key())).unwrap();

    // The re-shared v1 token and the keys its test verifies it with.
    let token = include_bytes!("../tests/fixtures/v1_reshared_token.bin");
    fs::write(trust.join("v1_reshared_token.bin"), token).unwrap();
    let (root, holder) = (NodeIdentity::from_secret_bytes(&[1; 32]), NodeIdentity::from_secret_bytes(&[3; 32]));
    fs::write(trust.join("v1_reshared_token.keys"), format!("root {}\nholder {}\n", root.public_key(), holder.public_key())).unwrap();

    // The grant tables after one save of each row kind, read back through the store.
    let sqlite = out("sqlite");
    fs::write(sqlite.join("grants.sql"), SCHEMA).unwrap();
    let store = GrantStore::with_connection(Connection::open_in_memory().unwrap()).unwrap();
    let spec = GrantSpec { scopes: vec![Scope::new(Resource::Session { id: "one".into() }, [Action::View]).unwrap()], ttl_ms: 60_000, reshare: true, until_ms: Some(NOW + 3_600_000) };
    let issued = IssuedGrant { id: GrantId("00112233445566778899aabbccddeeff".into()), holder: holder.public_key(), token: Vec::new(), expires_ms: NOW + 60_000 };
    store.insert_issued(&issued, &Definition::Root(spec), None, NOW).unwrap();
    store.revoke(&root.public_key(), &issued.id, NOW + 1).unwrap();
    store.record_revocation(&GrantId("ffeeddccbbaa99887766554433221100".into()), &root.public_key(), NOW + 2).unwrap();
    let received = store.receive(&holder.public_key(), &root.public_key(), token).unwrap();
    store.mark_refused(&received.id, NOW + 3).unwrap();
    assert_eq!(store.issued().unwrap().len(), 1);
    assert_eq!(store.received_all().unwrap()[0].refused_ms, Some(NOW + 3));
    let rows = dump(&store.db.locked(), &["grants_issued", "grants_received", "grants_revocations"]);
    fs::write(sqlite.join("grants.rows.json"), rows).unwrap();
}
