//! Golden files for the C++ port, written by `cargo test -p loa-p2p --lib
//! write_golden -- --ignored` into `$LOA_GOLDEN_OUT`. A child of `peers_db` so
//! the schema text is reachable.

use super::*;
use crate::wire::*;
use loa_core::{Dom, Draft, ElementKind, NodeId as DomNode, Value};
use loa_trust::Action;
use rusqlite::types::ValueRef;
use std::fs;
use std::path::PathBuf;
use tokio::io::AsyncReadExt;

fn out(sub: &str) -> PathBuf {
    let dir = PathBuf::from(std::env::var("LOA_GOLDEN_OUT").expect("LOA_GOLDEN_OUT")).join(sub);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// The bytes `wire::send` writes for `message`.
async fn framed<M: prost::Message>(message: &M) -> Vec<u8> {
    let (a, mut b) = tokio::io::duplex(2 * MAX_FRAME_SIZE);
    let mut tx: crate::transport::SendHalf = Box::new(a);
    send(&mut tx, message).await.unwrap();
    drop(tx);
    let mut bytes = Vec::new();
    b.read_to_end(&mut bytes).await.unwrap();
    bytes
}

#[tokio::test]
#[ignore = "writes golden files; run with LOA_GOLDEN_OUT set"]
async fn write_golden_wire_and_peers() {
    let p2p = out("p2p");
    let mut dom = Dom::new();
    let attrs = [("id", Value::Str("golden".into())), ("harness", Value::Str("omp".into())), ("node", Value::Str("k".into())), ("created", Value::Int(1))];
    let first = dom.commit(Draft::insert(DomNode::DOCUMENT, ElementKind::Session, attrs, ""), 1_700_000_000_000).unwrap();
    let second = dom.commit(Draft::append(first.node, "tok"), 1_700_000_000_001).unwrap();
    let wire_patch = |p: &loa_core::Patch| loa_bus::proto::Patch::from(p);
    let id = vec![0xab; 16];
    let token = vec![1, 2, 3, 4];

    let control = [
        ("hello", Control::Hello(Hello {
            app_version: "0.1.0".into(),
            protocols: vec![ProtocolVersion { name: "omp/control".into(), version: 1 }, ProtocolVersion { name: "omp/session".into(), version: 1 }, ProtocolVersion { name: "omp/folder".into(), version: 1 }],
            name: "golden".into(),
        })),
        ("presence", Control::Presence(Presence { online: true, last_seen_ms: Some(1_800_000_000_000) })),
        ("grant-delivery", Control::GrantDelivery(GrantDelivery { token: token.clone(), root: vec![9; 32] })),
        ("grant-renewal", Control::GrantRenewal(GrantRenewal { grant_id: id.clone() })),
        ("grant-renewal-result", Control::GrantRenewalResult(GrantRenewalResult { token: Some(token.clone()), refusal: String::new() })),
        ("grant-renewal-refused", Control::GrantRenewalResult(GrantRenewalResult { token: None, refusal: "revoked".into() })),
        ("grant-revocation", Control::GrantRevocation(GrantRevocation { grant_id: id.clone() })),
        ("ack", Control::Ack(Ack { error: String::new() })),
        ("ack-refused", Control::Ack(Ack { error: "no".into() })),
    ];
    for (name, body) in control {
        fs::write(p2p.join(format!("control-{name}.bin")), framed(&ControlFrame::from(body)).await).unwrap();
    }

    let frames = [
        ("subscribe", Body::Subscribe(Subscribe { session: "golden".into(), after_source_seq: 7 })),
        ("update", Body::Update(WireUpdate { source_seq: 2, patch: Some(wire_patch(&second)) })),
        ("drive", Body::Drive(Drive { session: "golden".into(), base_seq: 2, draft: br#"{"op":"append"}"#.to_vec(), ts: -5 })),
        ("challenge", Body::Challenge(Challenge { nonce: (0..32).collect() })),
        ("present", Body::Present(Present { grants: vec![PresentedGrant { token: token.clone(), proof: vec![0x5a; 64] }] })),
        ("list-sessions", Body::ListSessions(ListSessions {})),
        ("session-list", Body::SessionList(SessionList { sessions: vec![
            SessionEntry { id: "golden".into(), title: "Title ✓".into(), harness: "omp".into(), created_ms: 1_700_000_000_000, actions: vec!["view".into(), "prompt".into()], partial: false, last_seq: 2 },
            SessionEntry { id: "part".into(), title: String::new(), harness: "omp".into(), created_ms: 0, actions: vec!["view".into()], partial: true, last_seq: 0 },
        ] })),
        ("seed", Body::Seed(Seed::new(2, &[first.clone(), second.clone()], &[Action::View, Action::Drive]))),
        ("drive-result", Body::DriveResult(DriveResult { patch: Some(wire_patch(&second)), conflict_current: None, error: String::new() })),
        ("drive-result-conflict", Body::DriveResult(DriveResult { patch: None, conflict_current: Some(9), error: "conflict".into() })),
        ("prompt", Body::Prompt(Prompt { session: "golden".into(), text: "hello\nworld".into() })),
        ("ack", Body::Ack(Ack { error: String::new() })),
        ("ack-refused", Body::Ack(Ack { error: "no".into() })),
        ("answer", Body::Answer(Answer { session: "golden".into(), decision: 3, values: vec!["yes".into(), "a".into()], reason: Some("because".into()) })),
    ];
    for (name, body) in frames {
        fs::write(p2p.join(format!("frame-{name}.bin")), framed(&Frame::from(body)).await).unwrap();
    }

    let entry = WireEntry { name: "a.txt".into(), dir: false, size: 12 };
    let folder = [
        ("folder-request-list", framed(&FolderRequest { op: "list".into(), folder: "docs".into(), path: "sub".into(), size: 0, data: Vec::new() }).await),
        ("folder-request-write", framed(&FolderRequest { op: "write".into(), folder: "docs".into(), path: "a.txt".into(), size: 3, data: b"abc".to_vec() }).await),
        ("folder-reply-entries", framed(&FolderReply { error: String::new(), ready: true, entries: vec![entry.clone(), WireEntry { name: "sub".into(), dir: true, size: 0 }], data: Vec::new(), done: true }).await),
        ("folder-reply-refused", framed(&<FolderReply as Envelope>::ack(Some("denied".into()))).await),
        ("wire-entry", framed(&entry).await),
    ];
    for (name, bytes) in folder {
        fs::write(p2p.join(format!("{name}.bin")), bytes).unwrap();
    }
    let labels: Vec<&str> = LOCAL.iter().map(|l| std::str::from_utf8(l).unwrap()).collect();
    fs::write(p2p.join("labels.txt"), labels.join("\n") + "\n").unwrap();

    // net_peers after one save and an observed connection, read back.
    let sqlite = out("sqlite");
    fs::write(sqlite.join("net_peers.sql"), super::SCHEMA).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let peers = PeerDirectory::open(&dir.path().join("peers.sqlite")).unwrap();
    let key = iroh::SecretKey::from_bytes(&[7; 32]).public();
    let addresses: Vec<SocketAddr> = vec!["192.168.1.2:4433".parse().unwrap(), "[fe80::1]:4433".parse().unwrap()];
    peers.save(&key, Some("desk"), &addresses, DiscoverySource::Mdns).unwrap();
    assert!(peers.update(&key, "online = 1, last_seen_ms = ?2, app_version = ?3", &[&1_800_000_000_000_i64, &"0.1.0"]));
    let listed = peers.list();
    assert_eq!((listed.len(), listed[0].addresses.clone(), listed[0].last_seen_ms), (1, addresses, Some(1_800_000_000_000)));
    let db = peers.db.locked();
    let mut statement = db.prepare("SELECT * FROM net_peers ORDER BY rowid").unwrap();
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
                    ValueRef::Blob(_) => unreachable!("net_peers has no blob column"),
                };
                row.insert(name.clone(), value);
            }
            Ok(serde_json::Value::Object(row))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    fs::write(sqlite.join("net_peers.rows.json"), serde_json::to_string_pretty(&serde_json::json!({ "net_peers": rows })).unwrap() + "\n").unwrap();
}
