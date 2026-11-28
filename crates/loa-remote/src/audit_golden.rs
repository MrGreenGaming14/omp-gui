//! Golden files for the C++ port, written by `cargo test -p loa-remote --lib
//! write_golden -- --ignored` into `$LOA_GOLDEN_OUT/remote`. A child of `audit`
//! so the schema text is reachable.

use super::*;
use rusqlite::types::ValueRef;
use std::fs;
use std::path::PathBuf;

#[test]
#[ignore = "writes golden files; run with LOA_GOLDEN_OUT set"]
fn write_golden_remote_audit() {
    let out = PathBuf::from(std::env::var("LOA_GOLDEN_OUT").expect("LOA_GOLDEN_OUT")).join("remote");
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("remote_audit.sql"), SCHEMA).unwrap();

    let dir = tempfile::tempdir().unwrap();
    let audit = SqliteAudit::open(&dir.path().join("audit.db")).unwrap();
    let t0 = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
    let rows = [
        ("db", "s1", Action::Write, "/etc/app.conf", Outcome::Ok, "12 bytes"),
        ("db", "s2", Action::Command, "rm -rf /", Outcome::Refused, "blocked by policy"),
        ("web", "s1", Action::Terminal, "", Outcome::Denied, "user declined"),
        ("web", "s1", Action::Read, "/var/log/naïve ✓.log", Outcome::TimedOut, ""),
    ];
    for (i, (host, session, action, target, outcome, detail)) in rows.into_iter().enumerate() {
        let at = t0 + Duration::from_millis(i as u64 * 1001);
        let row = NewRecord { at, host: host.into(), session_id: session.into(), action, target: target.into(), outcome, detail: detail.into() };
        assert_eq!(audit.record(row).unwrap(), i as i64 + 1);
    }
    assert_eq!(audit.query(&AuditQuery::default()).unwrap().len(), 4);

    let db = audit.0.locked();
    let mut statement = db.prepare("SELECT * FROM remote_audit ORDER BY id").unwrap();
    let names: Vec<String> = statement.column_names().into_iter().map(str::to_owned).collect();
    let rows: Vec<serde_json::Value> = statement
        .query_map([], |r| {
            let mut row = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let value = match r.get_ref(i)? {
                    ValueRef::Integer(n) => n.into(),
                    ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned().into(),
                    other => unreachable!("remote_audit has only INTEGER and TEXT columns: {other:?}"),
                };
                row.insert(name.clone(), value);
            }
            Ok(serde_json::Value::Object(row))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    fs::write(out.join("remote_audit.rows.json"), serde_json::to_string_pretty(&serde_json::json!({ "remote_audit": rows })).unwrap() + "\n").unwrap();
}
