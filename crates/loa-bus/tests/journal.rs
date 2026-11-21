//! The on-disk journal: format, recovery, snapshots, forks, the store and
//! the journal writer.

mod common;

use common::{dom_at, every, file_len, flip_byte, history, journal_file, snapshot_files, write, written};
use loa_bus::{
    Journal, JournalError as Error, JournalOptions, JournalPersist, JournalWriter, Listing, NodeKey, Parent,
    SessionWriter, Store, SyncPolicy, WriterConfig,
};
use loa_core::testkit::DenyAll;
use loa_core::{Dom, Draft, ElementKind, Op, PatchSink, Value, attr};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn no_snapshots() -> JournalOptions {
    every(0)
}

fn set_len(path: &Path, len: u64) {
    OpenOptions::new().write(true).open(path).unwrap().set_len(len).unwrap();
}

#[test]
fn checked_in_journals_open_and_rewrite_byte_for_byte() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal");
    let files = |root: &Path| -> BTreeMap<PathBuf, Vec<u8>> {
        let entries = ["s", "f"].map(|id| fs::read_dir(root.join(id)).unwrap()).into_iter().flatten();
        entries
            .map(|e| e.unwrap().path())
            .filter(|p| !p.ends_with("lock"))
            .map(|p| (p.strip_prefix(root).unwrap().to_owned(), fs::read(&p).unwrap()))
            .collect()
    };
    let opened = tempfile::tempdir().unwrap();
    for (rel, bytes) in files(&fixtures) {
        fs::create_dir_all(opened.path().join(&rel).parent().unwrap()).unwrap();
        fs::write(opened.path().join(rel), bytes).unwrap();
    }
    let (s, cut) = Journal::open(&opened.path().join("s"), every(16)).unwrap();
    let patches: Vec<_> = s.patches(1).unwrap().map(Result::unwrap).collect();
    assert_eq!((cut, s.last_seq(), s.snapshot_seqs().collect::<Vec<_>>()), (None, 40, vec![16, 32]));
    assert_eq!(s.load().unwrap(), dom_at(&patches, 40));
    assert_eq!(s.load_at(20).unwrap(), dom_at(&patches, 20));
    let (f, _) = Journal::open(&opened.path().join("f"), every(16)).unwrap();
    assert_eq!((f.parent().cloned(), f.last_seq()), (Some(Parent { session_id: "s".into(), forked_at: 0 }), 0));

    // Writing the same patches again reproduces every byte.
    let again = tempfile::tempdir().unwrap();
    let (dir, _) = written(again.path(), "s", &patches, every(16));
    drop(Journal::open(&dir, every(16)).unwrap().0.fork(0, "f").unwrap());
    assert_eq!(files(again.path()), files(&fixtures));
}

#[test]
fn torn_tails_are_cut_and_other_damage_is_refused() {
    type Damage = fn(&Path, &[u64], u64) -> u64;
    type Refused = Option<fn(&Error) -> bool>;
    // Each gets the record offsets and the file length, damages the file and
    // returns its new length. Record 21 is the last.
    let cases: [(&str, Damage, Refused); 7] = [
        ("half a record", |p, o, e| (set_len(p, (o[20] + e) / 2), (o[20] + e) / 2).1, None),
        ("torn record header", |p, o, _| (set_len(p, o[20] + 3), o[20] + 3).1, None),
        ("zero filled tail", |p, o, _| (set_len(p, o[20]), set_len(p, o[20] + 4096), o[20] + 4096).2, None),
        ("bad checksum on the last record", |p, o, e| (flip_byte(p, (o[20] + e) / 2), e).1, None),
        ("damage before the last record", |p, o, e| (flip_byte(p, o[9] + 10), e).1, Some(|e| matches!(e, Error::Corrupt { .. }))),
        ("future version", |p, _, e| (flip_byte(p, 8), e).1, Some(|e| matches!(e, Error::UnsupportedVersion { .. }))),
        ("not a journal", |p, _, _| (fs::write(p, b"hello").unwrap(), 5).1, Some(|e| matches!(e, Error::NotAJournal(_)))),
    ];
    let (patches, _) = history("s", 21);
    for (name, damage, refused) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, offsets) = written(tmp.path(), "s", &patches, no_snapshots());
        let path = journal_file(&dir);
        let len = damage(&path, &offsets, file_len(&path));
        match (Journal::open(&dir, no_snapshots()), refused) {
            (Err(e), Some(expected)) => assert!(expected(&e), "{name}: {e}"),
            (Ok((mut journal, Some(cut))), None) => {
                assert_eq!((cut.last_seq, cut.offset, cut.offset + cut.dropped_bytes), (20, offsets[20], len), "{name}");
                assert_eq!(file_len(&path), offsets[20], "{name}");
                assert_eq!(journal.load().unwrap(), dom_at(&patches, 20), "{name}");
                // Writing resumes at the cut and survives a reopen.
                journal.append(&patches[20]).unwrap();
                journal.sync().unwrap();
                drop(journal);
                let (journal, cut) = Journal::open(&dir, no_snapshots()).unwrap();
                assert_eq!((cut, journal.load().unwrap()), (None, dom_at(&patches, 21)), "{name}");
            }
            (other, _) => panic!("{name}: {:?}", other.map(|(_, cut)| cut)),
        }
    }
}

#[test]
fn appends_must_follow_in_order_and_reads_start_anywhere() {
    let tmp = tempfile::tempdir().unwrap();
    let (patches, dom) = history("s", 1100);
    let mut journal = Journal::create(&tmp.path().join("s"), "s", every(700)).unwrap();
    assert!(matches!(journal.append(&patches[1]), Err(Error::SequenceGap { last: 0, got: 2 })));
    assert!(matches!(journal.append_applied(&patches[0], &Dom::new()), Err(Error::DomMismatch { dom: 0, journal: 1 })));
    write(&mut journal, &patches[..1099]);
    assert!(matches!(journal.append(&patches[0]), Err(Error::SequenceGap { last: 1099, got: 1 })));
    let before = journal.patches(1098).unwrap();
    write(&mut journal, &patches);
    assert_eq!(before.count(), 2, "an iterator sees the journal as it was");
    assert!(matches!(journal.write_snapshot(&dom_at(&patches, 9)), Err(Error::DomMismatch { dom: 9, .. })));
    for reopen in [false, true] {
        if reopen {
            drop(journal);
            journal = Journal::open(&tmp.path().join("s"), every(700)).unwrap().0;
        }
        for from in [0u64, 1, 700, 701, 1024, 1025, 1099, 1100, 1101] {
            let got: Vec<_> = journal.patches(from).unwrap().map(Result::unwrap).collect();
            assert_eq!(got, patches[from.max(1) as usize - 1..], "from {from}, reopened {reopen}");
        }
        assert!(matches!(journal.patches(1102), Err(Error::OutOfRange { seq: 1102, last: 1100 })));
        assert_eq!((journal.load().unwrap(), journal.load_at(0).unwrap()), (dom.clone(), Dom::new()));
    }
}

#[test]
fn damaged_snapshots_fall_back_to_older_ones_then_to_a_full_replay() {
    type Damage = fn(&Path, &[PathBuf]);
    // Record 10 is damaged in every case, so loading must not replay it.
    let cases: [(&str, Damage, Option<&[u64]>, bool); 6] = [
        ("intact", |_, _| {}, Some(&[100, 200]), true),
        ("newest body", |_, s| flip_byte(&s[1], file_len(&s[1]) - 1), Some(&[100, 200]), true),
        ("newest header", |_, s| flip_byte(&s[1], 13), Some(&[100]), true),
        ("temp file from a crash", |d, _| fs::write(d.join("snap-00000000000000000250.loas.tmp"), b"LOASNAP\0").unwrap(), Some(&[100, 200]), true),
        ("both bodies", |_, s| s.iter().for_each(|s| flip_byte(s, file_len(s) - 1)), Some(&[100, 200]), false),
        ("none left", |_, s| s.iter().for_each(|s| fs::remove_file(s).unwrap()), None, false),
    ];
    let (patches, dom) = history("s", 250);
    for (name, damage, seqs, loads) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let (dir, offsets) = written(tmp.path(), "s", &patches, every(100));
        flip_byte(&journal_file(&dir), offsets[9] + 10);
        damage(&dir, &snapshot_files(&dir));
        let Ok((journal, _)) = Journal::open(&dir, every(100)) else {
            assert_eq!(seqs, None, "{name}: open failed");
            continue;
        };
        assert_eq!(journal.snapshot_seqs().collect::<Vec<_>>(), seqs.unwrap(), "{name}");
        assert!(fs::read_dir(&dir).unwrap().all(|e| !e.unwrap().path().to_string_lossy().ends_with(".tmp")));
        match journal.load() {
            Ok(loaded) => assert!(loads && loaded == dom, "{name}"),
            Err(e) => assert!(!loads && matches!(e, Error::Corrupt { .. }), "{name}: {e}"),
        }
        assert_eq!(journal.load_at(230).ok(), loads.then(|| dom_at(&patches, 230)), "{name}");
    }
    // With an intact journal, damage to every snapshot ends in a full replay.
    let tmp = tempfile::tempdir().unwrap();
    let (dir, _) = written(tmp.path(), "s", &patches, JournalOptions { snapshot_every: 60, keep_snapshots: 3 });
    assert_eq!(snapshot_files(&dir).len(), 3, "older snapshots are pruned");
    snapshot_files(&dir).iter().for_each(|s| flip_byte(s, file_len(s) - 1));
    assert_eq!(Journal::open(&dir, every(100)).unwrap().0.load().unwrap(), dom);
}

#[test]
fn forks_copy_their_history_and_link_their_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path().join("sessions"), every(16)).unwrap();
    let (patches, dom) = history("src", 50);
    let mut src = store.create("src").unwrap();
    write(&mut src, &patches);
    let contents = |dir: &Path| fs::read_dir(dir).unwrap().map(|e| fs::read(e.unwrap().path()).unwrap()).collect::<Vec<_>>();
    let before = contents(src.dir());

    let mut b = src.fork(20, "b").unwrap();
    assert_eq!(b.parent(), Some(&Parent { session_id: "src".into(), forked_at: 20 }));
    assert_eq!(b.patches(1).unwrap().take(20).map(Result::unwrap).collect::<Vec<_>>(), patches[..20]);
    let mut b_dom = b.load().unwrap();
    let root = |dom: &Dom, name| dom.root().and_then(|r| r.attr(name)).cloned();
    assert_eq!(b.last_seq(), 23, "the fork relabels its root");
    assert_eq!([attr::ID, attr::PARENT_SESSION].map(|a| root(&b_dom, a)), [Some("b".into()), Some("src".into())]);
    assert_eq!(root(&b_dom, attr::FORKED_AT), Some(Value::Int(20)));
    let ext = b_dom.iter().find(|n| n.kind == ElementKind::Ext).unwrap().id;
    for i in 0..10i64 {
        let p = b_dom.commit(Draft::set(ext, "x-b", i), 9).unwrap();
        b.append_applied(&p, &b_dom).unwrap();
    }
    let c = b.fork(30, "c").unwrap();
    assert_eq!((c.parent().unwrap().session_id.as_str(), c.load_at(30).unwrap()), ("b", b.load_at(30).unwrap()));
    assert_eq!(c.load_at(20).unwrap(), dom_at(&patches, 20), "c carries the source's history too");
    let reopened = { drop(b); store.open("b").unwrap().0.load().unwrap() };
    assert_eq!(reopened, b_dom);

    assert_eq!(src.fork(0, "empty").unwrap().load().unwrap(), Dom::new());
    assert_eq!(src.fork(50, "whole").unwrap().load_at(50).unwrap(), dom);
    assert!(matches!(src.fork(51, "late"), Err(Error::OutOfRange { seq: 51, last: 50 })));
    assert!(matches!(src.fork(10, "c"), Err(Error::Exists(id)) if id == "c"));
    assert!(!store.root().join("late").exists());
    assert_eq!(contents(src.dir()), before, "forking never touches the source");

    drop((src, c));
    let lineage: BTreeMap<_, _> = store
        .list()
        .unwrap()
        .into_iter()
        .map(|l| match l {
            Listing::Session(i) => (i.id, (i.parent.map(|p| (p.session_id, p.forked_at)), i.last_seq)),
            Listing::Unreadable { id, error } => panic!("{id}: {error}"),
        })
        .collect();
    assert_eq!(lineage["c"], (Some(("b".into(), 30)), 33));
    assert_eq!(lineage["src"], (None, 50));
    store.delete("src").unwrap();
    assert!(matches!(store.open("src"), Err(Error::NotFound(_))));
    assert_eq!(store.open("c").unwrap().0.load_at(20).unwrap(), dom_at(&patches, 20), "forks outlive their source");
}

#[test]
fn an_open_session_is_locked_and_bad_ids_and_files_are_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path().join("s"), every(16)).unwrap();
    for bad in ["", "..", "../x", "a/b", ".hidden", "a\\b", "a b"] {
        assert!(matches!(store.create(bad), Err(Error::InvalidSessionId(_))), "{bad:?}");
    }
    let open = store.create("ok").unwrap();
    assert!(matches!(store.create("ok"), Err(Error::Exists(_))));
    assert!(matches!(store.open("ok"), Err(Error::Locked(_))));
    assert!(matches!(store.delete("ok"), Err(Error::Locked(_))));
    fs::create_dir(store.root().join("broken")).unwrap();
    fs::write(journal_file(&store.root().join("broken")), b"not a journal").unwrap();
    let listing = store.list().unwrap();
    assert!(matches!(&listing[..], [Listing::Unreadable { error: Error::NotAJournal(_), .. }, Listing::Session(i)] if i.id == "ok"));
    drop(open);
    drop(store.open("ok").unwrap());
    store.delete("ok").unwrap();
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn the_journal_writer_syncs_as_its_strategy_says() {
    let tmp = tempfile::tempdir().unwrap();
    let (patches, dom) = history("w", 12);
    let hour = Duration::from_secs(3600);
    let cases = [
        // Synced at 4 and 8; the snapshot at 10 syncs too.
        (SyncPolicy::Batch { max_patches: 4, max_delay: hour }, every(10), vec![1, 2, 3, 0, 1, 2, 3, 0, 1, 0, 1, 2]),
        (SyncPolicy::Batch { max_patches: u32::MAX, max_delay: Duration::ZERO }, no_snapshots(), vec![0; 12]),
        (SyncPolicy::EveryPatch, no_snapshots(), vec![0; 12]),
        (SyncPolicy::Manual, no_snapshots(), (1..=12).collect()),
    ];
    for (i, (policy, options, expected)) in cases.into_iter().enumerate() {
        let id = format!("w{i}");
        let writer = JournalWriter::new(Journal::create(&tmp.path().join(&id), &id, options).unwrap(), policy);
        let mut mirror = Dom::new();
        let pending: Vec<_> = patches
            .iter()
            .map(|p| {
                mirror.apply(p).unwrap();
                writer.append(p, &mirror).unwrap();
                writer.pending()
            })
            .collect();
        assert_eq!(pending, expected, "{policy:?}");
        writer.sync().unwrap();
        assert_eq!(writer.pending(), 0);
        writer.close().unwrap();
        assert!(matches!(writer.append(&patches[0], &mirror), Err(Error::Closed)));
        assert_eq!(Journal::open(&tmp.path().join(&id), no_snapshots()).unwrap().0.load().unwrap(), dom);
    }
}

#[test]
fn a_session_writer_persists_through_the_journal_and_snapshots_from_its_own_dom() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::new(tmp.path(), JournalOptions { snapshot_every: 5, keep_snapshots: 2 }).unwrap();
    let journal = Arc::new(JournalWriter::new(store.create("s1").unwrap(), SyncPolicy::Manual));
    let key = NodeKey([7; 32]);
    let persist = Box::new(JournalPersist(Arc::clone(&journal)));
    let config = WriterConfig { session: "s1".into(), owner: key, local: key, dom: Dom::new(), persist, authorize: Arc::new(DenyAll) };
    let writer = SessionWriter::new(config);
    let (patches, dom) = history("s1", 23);
    for p in &patches {
        let draft = match p.op.clone() {
            Op::Insert { parent, index, kind, attrs, text } => Draft::Insert { parent, index, kind, attrs, text },
            Op::Set { name, value } => Draft::Set { node: p.node, name, value },
            Op::Append { text } => Draft::Append { node: p.node, text },
            Op::Remove => Draft::Remove { node: p.node },
        };
        writer.commit_at(draft, p.ts).unwrap();
    }
    // Snapshots at 5, 10, 15 and 20, two kept, each syncing the journal.
    assert_eq!((journal.last_seq(), journal.pending(), snapshot_files(&tmp.path().join("s1")).len()), (23, 3, 2));
    assert_eq!(journal.load_at(12).unwrap(), dom_at(&patches, 12));
    drop((writer, journal));
    let (reopened, cut) = store.open("s1").unwrap();
    assert_eq!((cut, reopened.snapshot_seqs().collect::<Vec<_>>()), (None, vec![15, 20]));
    assert_eq!(reopened.load().unwrap(), dom);
}
