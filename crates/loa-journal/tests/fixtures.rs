//! On-disk format guard: files written by the original loa-journal are
//! checked in and must keep opening, and new writes must match them byte for byte.

mod common;

use common::{Session, write};
use loa_core::{Draft, ElementKind, Value, attr};
use loa_journal::{Journal, JournalOptions};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../loa-bus/tests/fixtures/journal")
}

fn opts() -> JournalOptions {
    JournalOptions { snapshot_every: 16, keep_snapshots: 2 }
}

fn session() -> Session {
    let mut s = Session::new("s");
    s.commit(Draft::insert(
        s.root,
        ElementKind::Ext,
        [(attr::HARNESS, Value::from("x")), ("x-neg", Value::Int(-42)), ("x-raw", Value::Bytes((0..=255).collect()))],
        "naïve \u{1F600}",
    ));
    s.grow_to(40);
    s
}

/// Writes session `s` (snapshots at 16 and 32) and its empty fork `f` under `root`.
fn generate(root: &Path) -> Session {
    let s = session();
    let mut journal = Journal::create(&root.join("s"), "s", opts()).unwrap();
    write(&mut journal, &s);
    drop(journal.fork(0, "f").unwrap());
    s
}

/// Every file except the lock, by path relative to `root`.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for id in ["s", "f"] {
        for e in fs::read_dir(root.join(id)).unwrap() {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            if name != "lock" {
                out.insert(format!("{id}/{name}"), fs::read(e.path()).unwrap());
            }
        }
    }
    out
}

#[test]
fn checked_in_journals_open_and_new_writes_match_them_byte_for_byte() {
    if std::env::var_os("LOA_WRITE_FIXTURES").is_some() {
        let _ = fs::remove_dir_all(fixtures());
        fs::create_dir_all(fixtures()).unwrap();
        generate(&fixtures());
        for id in ["s", "f"] {
            fs::remove_file(fixtures().join(id).join("lock")).unwrap();
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let expected = generate(tmp.path());
    assert_eq!(files(tmp.path()), files(&fixtures()));

    let copy = tempfile::tempdir().unwrap();
    for (rel, bytes) in files(&fixtures()) {
        let path = copy.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let (s, cut) = Journal::open(&copy.path().join("s"), opts()).unwrap();
    assert_eq!(cut, None);
    assert_eq!(s.snapshot_seqs().collect::<Vec<_>>(), [16, 32]);
    assert_eq!(s.load().unwrap(), expected.dom);
    assert_eq!(s.load_at(20).unwrap(), expected.dom_at(20));
    let (f, _) = Journal::open(&copy.path().join("f"), opts()).unwrap();
    let parent = f.parent().unwrap();
    assert_eq!((parent.session_id.as_str(), parent.forked_at, f.last_seq()), ("s", 0, 0));
}
