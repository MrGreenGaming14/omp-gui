mod common;

use common::{Session, write};
use loa_core::{Dom, Draft, Value, attr};
use loa_journal::{Error, Journal, JournalOptions, JournalWriter, Listing, Parent, Store, SyncPolicy};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

fn opts() -> JournalOptions {
    JournalOptions { snapshot_every: 16, keep_snapshots: 2 }
}

fn root_attr<'a>(dom: &'a Dom, name: &str) -> Option<&'a Value> {
    dom.root().and_then(|r| r.attr(name))
}

fn dir_contents(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_string_lossy().into_owned(), fs::read(e.path()).unwrap())
        })
        .collect()
}

/// A store holding session `src` with 50 patches.
fn store_with_source() -> (tempfile::TempDir, Store, Journal, Session) {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path().join("sessions"), opts()).unwrap();
    let mut session = Session::new("src");
    session.grow_to(50);
    let mut journal = store.create("src").unwrap();
    write(&mut journal, &session);
    (tmp, store, journal, session)
}

#[test]
fn fork_matches_the_source_at_the_position_and_continues_numbering() {
    let (_tmp, _store, source, session) = store_with_source();
    let mut fork = source.fork(20, "fork1").unwrap();

    assert_eq!(fork.session_id(), "fork1");
    assert_eq!(fork.parent(), Some(&Parent { session_id: "src".into(), forked_at: 20 }));
    assert_eq!(fork.load_at(20).unwrap(), session.dom_at(20));
    let shared: Vec<_> = fork.patches(1).unwrap().take(20).map(Result::unwrap).collect();
    assert_eq!(shared, session.patches[..20]);

    // Its own first patches relabel the root.
    assert_eq!(fork.last_seq(), 23);
    let dom = fork.load().unwrap();
    assert_eq!(root_attr(&dom, attr::ID), Some(&Value::from("fork1")));
    assert_eq!(root_attr(&dom, attr::PARENT_SESSION), Some(&Value::from("src")));
    assert_eq!(root_attr(&dom, attr::FORKED_AT), Some(&Value::Int(20)));
    assert_eq!(dom.len(), session.dom_at(20).len());

    // Writing continues from 24 and survives a reopen.
    let mut writer_dom = dom;
    let patch = writer_dom.commit(Draft::append(session.turn, "x"), 5).unwrap();
    assert_eq!(patch.seq, 24);
    fork.append_applied(&patch, &writer_dom).unwrap();
    let dir = fork.dir().to_owned();
    drop(fork);
    let (fork, truncation) = Journal::open(&dir, opts()).unwrap();
    assert_eq!(truncation, None);
    assert_eq!(fork.parent(), Some(&Parent { session_id: "src".into(), forked_at: 20 }));
    assert_eq!(fork.load().unwrap(), writer_dom);
}

#[test]
fn forking_never_modifies_the_source() {
    let (_tmp, _store, source, session) = store_with_source();
    let before = dir_contents(source.dir());
    let fork = source.fork(33, "f").unwrap();
    drop(fork);
    assert_eq!(dir_contents(source.dir()), before);
    assert_eq!(source.last_seq(), 50);
    assert_eq!(source.load().unwrap(), session.dom);
}

#[test]
fn fork_of_a_fork_links_to_its_direct_parent() {
    let (_tmp, store, source, session) = store_with_source();
    let mut b = source.fork(20, "b").unwrap();
    let mut b_dom = b.load().unwrap();
    for i in 0..10 {
        let p = b_dom.commit(Draft::append(session.turn, format!("b{i}")), 9).unwrap();
        b.append_applied(&p, &b_dom).unwrap();
    }
    assert_eq!(b.last_seq(), 33);
    let c = b.fork(30, "c").unwrap();
    assert_eq!(c.parent(), Some(&Parent { session_id: "b".into(), forked_at: 30 }));
    assert_eq!(c.load_at(30).unwrap(), b.load_at(30).unwrap());
    let c_dom = c.load().unwrap();
    assert_eq!(c.last_seq(), 33);
    assert_eq!(root_attr(&c_dom, attr::ID), Some(&Value::from("c")));
    assert_eq!(root_attr(&c_dom, attr::PARENT_SESSION), Some(&Value::from("b")));
    assert_eq!(root_attr(&c_dom, attr::FORKED_AT), Some(&Value::Int(30)));
    // c carries the original session's history too.
    assert_eq!(c.load_at(20).unwrap(), session.dom_at(20));
    drop((b, c));

    let parents: BTreeMap<_, _> = store
        .list()
        .unwrap()
        .into_iter()
        .map(|l| match l {
            Listing::Session(info) => (info.id, (info.parent.map(|p| (p.session_id, p.forked_at)), info.last_seq)),
            Listing::Unreadable { id, error } => panic!("{id}: {error}"),
        })
        .collect();
    assert_eq!(
        parents,
        BTreeMap::from([
            ("b".to_owned(), (Some(("src".to_owned(), 20)), 33)),
            ("c".to_owned(), (Some(("b".to_owned(), 30)), 33)),
            ("src".to_owned(), (None, 50)),
        ])
    );
}

#[test]
fn fork_at_the_start_and_at_the_end() {
    let (_tmp, _store, source, session) = store_with_source();
    let empty = source.fork(0, "empty").unwrap();
    assert_eq!(empty.last_seq(), 0);
    assert_eq!(empty.load().unwrap(), Dom::new());
    let whole = source.fork(50, "whole").unwrap();
    assert_eq!(whole.load_at(50).unwrap(), session.dom);
    assert!(matches!(source.fork(51, "late"), Err(Error::OutOfRange { seq: 51, last: 50 })));
    assert!(!source.dir().parent().unwrap().join("late").exists());
}

#[test]
fn fork_into_an_existing_session_fails_and_leaves_it_alone() {
    let (_tmp, store, source, _session) = store_with_source();
    let mut other = Session::new("other");
    other.grow_to(5);
    let mut j = store.create("other").unwrap();
    write(&mut j, &other);
    drop(j);
    assert!(matches!(source.fork(10, "other"), Err(Error::Exists(id)) if id == "other"));
    let (j, _) = store.open("other").unwrap();
    assert_eq!(j.load().unwrap(), other.dom);
}

#[test]
fn fork_outlives_its_deleted_source() {
    let (_tmp, store, source, session) = store_with_source();
    drop(source.fork(40, "keep").unwrap());
    drop(source);
    store.delete("src").unwrap();
    assert!(matches!(store.open("src"), Err(Error::NotFound(_))));
    let (fork, _) = store.open("keep").unwrap();
    assert_eq!(fork.load_at(40).unwrap(), session.dom_at(40));
}

#[test]
fn an_open_session_is_locked_against_a_second_writer_and_delete() {
    let (_tmp, store, source, _session) = store_with_source();
    assert!(matches!(store.open("src"), Err(Error::Locked(_))));
    assert!(matches!(store.delete("src"), Err(Error::Locked(_))));
    drop(source);
    let (j, _) = store.open("src").unwrap();
    drop(j);
    store.delete("src").unwrap();
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn session_ids_that_escape_the_store_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path().join("s"), opts()).unwrap();
    for bad in ["", "..", "../x", "a/b", ".hidden", "a\\b", "a b"] {
        assert!(matches!(store.create(bad), Err(Error::InvalidSessionId(_))), "{bad:?}");
    }
    assert!(store.create("ok").is_ok());
    assert!(matches!(store.create("ok"), Err(Error::Exists(_))));
    assert!(fs::read_dir(tmp.path()).unwrap().count() == 1);
}

#[test]
fn list_marks_unreadable_sessions_without_hiding_the_rest() {
    let (_tmp, store, source, _session) = store_with_source();
    drop(source);
    let broken = store.root().join("broken");
    fs::create_dir(&broken).unwrap();
    fs::write(broken.join(loa_journal::JOURNAL_FILE), b"not a journal at all").unwrap();
    let listing = store.list().unwrap();
    assert_eq!(listing.len(), 2);
    assert!(matches!(&listing[0], Listing::Unreadable { id, error: Error::NotAJournal(_) } if id == "broken"));
    assert!(matches!(&listing[1], Listing::Session(info) if info.id == "src" && info.last_seq == 50));
}

#[test]
fn writer_batches_fsyncs_by_count() {
    let tmp = tempfile::tempdir().unwrap();
    let mut session = Session::new("w");
    session.grow_to(12);
    let journal = Journal::create(&tmp.path().join("w"), "w", JournalOptions { snapshot_every: 10, keep_snapshots: 2 })
        .unwrap();
    let writer =
        JournalWriter::new(journal, SyncPolicy::Batch { max_patches: 4, max_delay: Duration::from_secs(3600) });
    let mut dom = Dom::new();
    let mut pending = Vec::new();
    for p in &session.patches {
        dom.apply(p).unwrap();
        writer.append(p, &dom).unwrap();
        pending.push(writer.pending());
    }
    // Synced at 4 and 8, the snapshot at 10 syncs too.
    assert_eq!(pending, [1, 2, 3, 0, 1, 2, 3, 0, 1, 0, 1, 2]);
    assert_eq!(writer.patches(11).unwrap().count(), 2);
    let journal = writer.into_journal().unwrap();
    assert!(!journal.has_unsynced());
    assert_eq!(journal.snapshot_seqs().collect::<Vec<_>>(), [10]);
    assert_eq!(journal.load().unwrap(), session.dom);
}

#[test]
fn writer_syncs_by_age_every_patch_or_on_request() {
    let tmp = tempfile::tempdir().unwrap();
    let mut session = Session::new("w");
    session.grow_to(4);
    // Feeds patches `0..n` to `writer` and returns its pending count after each.
    let feed = |writer: &JournalWriter, n: usize| -> Vec<u32> {
        let mut dom = Dom::new();
        session.patches[..n]
            .iter()
            .map(|p| {
                dom.apply(p).unwrap();
                writer.append(p, &dom).unwrap();
                writer.pending()
            })
            .collect()
    };
    let no_snap = JournalOptions { snapshot_every: 0, keep_snapshots: 1 };
    let writer = |name: &str, policy| {
        JournalWriter::new(Journal::create(&tmp.path().join(name), name, no_snap.clone()).unwrap(), policy)
    };

    let aged = writer("a", SyncPolicy::Batch { max_patches: u32::MAX, max_delay: Duration::ZERO });
    assert_eq!(feed(&aged, 2), [0, 0]);

    let every = writer("b", SyncPolicy::EveryPatch);
    assert_eq!(feed(&every, 2), [0, 0]);

    let manual = writer("c", SyncPolicy::Manual);
    assert_eq!(feed(&manual, 3), [1, 2, 3]);
    manual.sync().unwrap();
    assert_eq!(manual.pending(), 0);
}
