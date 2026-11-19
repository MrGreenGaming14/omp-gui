mod common;

use common::{Session, file_len, flip_byte, journal_file, snapshot_files, write};
use loa_core::{Dom, Draft, ElementKind, NodeId, Value, attr};
use loa_journal::{Error, Journal, JournalOptions, Truncation};
use std::fs::{self, OpenOptions};
use std::io::Write;

fn no_snapshots() -> JournalOptions {
    JournalOptions { snapshot_every: 0, keep_snapshots: 2 }
}

fn every(n: u64) -> JournalOptions {
    JournalOptions { snapshot_every: n, keep_snapshots: 2 }
}

#[test]
fn reopened_journal_replays_to_the_writers_dom() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s1");
    let mut session = Session::new("s1");
    session.grow_to(500);
    let mut journal = Journal::create(&dir, "s1", no_snapshots()).unwrap();
    write(&mut journal, &session);
    drop(journal);

    let (journal, truncation) = Journal::open(&dir, no_snapshots()).unwrap();
    assert_eq!(truncation, None);
    assert_eq!(journal.session_id(), "s1");
    assert_eq!(journal.parent(), None);
    assert_eq!(journal.last_seq(), 500);
    assert_eq!(journal.load().unwrap(), session.dom);
    assert_eq!(journal.load_at(137).unwrap(), session.dom_at(137));
    assert_eq!(journal.load_at(0).unwrap(), Dom::new());
}

#[test]
fn append_rejects_gaps_and_repeats() {
    let tmp = tempfile::tempdir().unwrap();
    let mut session = Session::new("s");
    session.grow_to(5);
    let mut journal = Journal::create(&tmp.path().join("s"), "s", no_snapshots()).unwrap();
    assert!(matches!(journal.append(&session.patches[1]), Err(Error::SequenceGap { last: 0, got: 2 })));
    journal.append(&session.patches[0]).unwrap();
    assert!(matches!(journal.append(&session.patches[0]), Err(Error::SequenceGap { last: 1, got: 1 })));
    assert!(matches!(journal.append(&session.patches[2]), Err(Error::SequenceGap { last: 1, got: 3 })));
    journal.append(&session.patches[1]).unwrap();
    assert_eq!(journal.last_seq(), 2);
    assert_eq!(journal.load().unwrap(), session.dom_at(2));
}

#[test]
fn append_applied_rejects_a_dom_that_is_not_at_the_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Session::new("s");
    let mut journal = Journal::create(&tmp.path().join("s"), "s", no_snapshots()).unwrap();
    let stale = Dom::new();
    assert!(matches!(
        journal.append_applied(&session.patches[0], &stale),
        Err(Error::DomMismatch { dom: 0, journal: 1 })
    ));
    assert_eq!(journal.last_seq(), 0);
}

/// Journal with 20 patches on disk plus the 21st cut off by `damage`, which
/// gets the journal path and the 21st record's start and end offsets.
fn torn(damage: impl FnOnce(&std::path::Path, u64, u64)) -> (tempfile::TempDir, Session, Option<Truncation>, Journal) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let mut session = Session::new("s");
    session.grow_to(21);
    let mut journal = Journal::create(&dir, "s", no_snapshots()).unwrap();
    let offsets = write(&mut journal, &session);
    drop(journal);
    let path = journal_file(&dir);
    damage(&path, offsets[20], file_len(&path));
    let (journal, truncation) = Journal::open(&dir, no_snapshots()).unwrap();
    (tmp, session, truncation, journal)
}

fn assert_cut_to_20(session: &Session, truncation: Option<Truncation>, journal: &Journal, len_before: u64) {
    let t = truncation.expect("torn tail must be reported");
    assert_eq!(t.last_seq, 20);
    assert_eq!(journal.last_seq(), 20);
    assert_eq!(file_len(&journal_file(journal.dir())), t.offset);
    assert_eq!(t.offset + t.dropped_bytes, len_before);
    assert_eq!(journal.load().unwrap(), session.dom_at(20));
}

#[test]
fn half_written_last_record_is_cut_and_reported() {
    let mut len_before = 0;
    let (_tmp, session, truncation, mut journal) = torn(|path, start, end| {
        let f = OpenOptions::new().write(true).open(path).unwrap();
        len_before = start + (end - start) / 2;
        f.set_len(len_before).unwrap();
    });
    assert_cut_to_20(&session, truncation, &journal, len_before);

    // Writing resumes at the cut and survives another reopen.
    journal.append(&session.patches[20]).unwrap();
    journal.sync().unwrap();
    let dir = journal.dir().to_owned();
    drop(journal);
    let (journal, truncation) = Journal::open(&dir, no_snapshots()).unwrap();
    assert_eq!(truncation, None);
    assert_eq!(journal.load().unwrap(), session.dom_at(21));
}

#[test]
fn torn_header_of_last_record_is_cut() {
    let mut len_before = 0;
    let (_tmp, session, truncation, journal) = torn(|path, start, _| {
        let f = OpenOptions::new().write(true).open(path).unwrap();
        len_before = start + 3;
        f.set_len(len_before).unwrap();
    });
    assert_cut_to_20(&session, truncation, &journal, len_before);
}

#[test]
fn zero_filled_tail_is_cut() {
    let mut len_before = 0;
    let (_tmp, session, truncation, journal) = torn(|path, start, _| {
        let f = OpenOptions::new().write(true).open(path).unwrap();
        f.set_len(start).unwrap();
        f.set_len(start + 4096).unwrap();
        len_before = start + 4096;
    });
    assert_cut_to_20(&session, truncation, &journal, len_before);
}

#[test]
fn bad_checksum_on_the_last_record_is_a_torn_write() {
    let mut len_before = 0;
    let (_tmp, session, truncation, journal) = torn(|path, start, end| {
        flip_byte(path, (start + end) / 2);
        len_before = end;
    });
    assert_cut_to_20(&session, truncation, &journal, len_before);
}

#[test]
fn damage_before_the_last_record_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let mut session = Session::new("s");
    session.grow_to(30);
    let mut journal = Journal::create(&dir, "s", no_snapshots()).unwrap();
    let offsets = write(&mut journal, &session);
    drop(journal);
    let path = journal_file(&dir);
    let len = file_len(&path);
    flip_byte(&path, offsets[9] + 10);
    match Journal::open(&dir, no_snapshots()) {
        Err(Error::Corrupt { offset, .. }) => assert_eq!(offset, offsets[9]),
        other => panic!("expected corruption, got {:?}", other.map(|_| ())),
    }
    // Nothing was cut.
    assert_eq!(file_len(&path), len);
}

#[test]
fn not_a_journal_and_future_versions_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    drop(Journal::create(&dir, "s", no_snapshots()).unwrap());
    let path = journal_file(&dir);
    let mut bytes = fs::read(&path).unwrap();
    bytes[8] = 9;
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(Journal::open(&dir, no_snapshots()), Err(Error::UnsupportedVersion { found: 9, .. })));
    fs::write(&path, b"hello").unwrap();
    assert!(matches!(Journal::open(&dir, no_snapshots()), Err(Error::NotAJournal(_))));
}

#[test]
fn patches_from_any_position_match_what_was_appended() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let mut session = Session::new("s");
    session.grow_to(3000);
    let mut journal = Journal::create(&dir, "s", every(700)).unwrap();
    write(&mut journal, &session);
    for reopen in [false, true] {
        if reopen {
            drop(journal);
            journal = Journal::open(&dir, every(700)).unwrap().0;
        }
        for from in [0u64, 1, 2, 700, 701, 1024, 1025, 1500, 2100, 2999, 3000, 3001] {
            let got: Vec<_> = journal.patches(from).unwrap().map(Result::unwrap).collect();
            let start = from.max(1) as usize - 1;
            assert_eq!(got, session.patches[start..], "from {from}, reopened {reopen}");
        }
        assert!(matches!(journal.patches(3002), Err(Error::OutOfRange { seq: 3002, last: 3000 })));
    }

    // An iterator is a view of the journal when it was made.
    let mut more = session.clone();
    more.grow_to(3001);
    let before = journal.patches(2999).unwrap();
    write(&mut journal, &more);
    assert_eq!(before.count(), 2);
}

#[test]
fn snapshots_are_written_every_n_patches_and_old_ones_pruned() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let mut session = Session::new("s");
    session.grow_to(350);
    let mut journal = Journal::create(&dir, "s", every(100)).unwrap();
    write(&mut journal, &session);
    assert_eq!(journal.snapshot_seqs().collect::<Vec<_>>(), [200, 300]);
    assert_eq!(snapshot_files(&dir).len(), 2);
    drop(journal);
    let (journal, _) = Journal::open(&dir, every(100)).unwrap();
    assert_eq!(journal.snapshot_seqs().collect::<Vec<_>>(), [200, 300]);
    assert_eq!(journal.load().unwrap(), session.dom);
}

/// Session of 250 patches with snapshots at 100 and 200, then record 10
/// damaged on disk.
fn snapshotted_with_damaged_start() -> (tempfile::TempDir, std::path::PathBuf, Session) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let mut session = Session::new("s");
    session.grow_to(250);
    let mut journal = Journal::create(&dir, "s", every(100)).unwrap();
    let offsets = write(&mut journal, &session);
    drop(journal);
    flip_byte(&journal_file(&dir), offsets[9] + 10);
    (tmp, dir, session)
}

#[test]
fn load_after_a_snapshot_does_not_read_earlier_records() {
    let (_tmp, dir, session) = snapshotted_with_damaged_start();
    // Open and load only touch records after the snapshot at 200, so the
    // damaged record 10 goes unnoticed.
    let (journal, truncation) = Journal::open(&dir, every(100)).unwrap();
    assert_eq!(truncation, None);
    assert_eq!(journal.load().unwrap(), session.dom);
    assert_eq!(journal.load_at(230).unwrap(), session.dom_at(230));
    // Reading from the start does hit it.
    let read: Vec<_> = journal.patches(1).unwrap().collect();
    assert_eq!(read.len(), 10);
    assert!(matches!(read[9], Err(Error::Corrupt { .. })));
    drop(journal);

    // Without snapshots the same journal can't even be opened.
    for snap in snapshot_files(&dir) {
        fs::remove_file(snap).unwrap();
    }
    assert!(matches!(Journal::open(&dir, every(100)), Err(Error::Corrupt { .. })));
}

#[test]
fn damaged_snapshot_falls_back_to_the_previous_one() {
    let (_tmp, dir, session) = snapshotted_with_damaged_start();
    let snaps = snapshot_files(&dir);
    assert_eq!(snaps.len(), 2);
    let newest = &snaps[1];
    flip_byte(newest, file_len(newest) - 1);
    let (journal, _) = Journal::open(&dir, every(100)).unwrap();
    // Loaded from the snapshot at 100: a full replay would hit record 10.
    assert_eq!(journal.load().unwrap(), session.dom);
    drop(journal);

    // Both damaged: the load replays from the start and runs into record 10.
    flip_byte(&snaps[0], file_len(&snaps[0]) - 1);
    let (journal, _) = Journal::open(&dir, every(100)).unwrap();
    assert!(matches!(journal.load(), Err(Error::Corrupt { .. })));
}

#[test]
fn snapshot_with_damaged_header_is_skipped_on_open() {
    let (_tmp, dir, session) = snapshotted_with_damaged_start();
    let snaps = snapshot_files(&dir);
    flip_byte(&snaps[1], 13);
    let (journal, _) = Journal::open(&dir, every(100)).unwrap();
    assert_eq!(journal.snapshot_seqs().collect::<Vec<_>>(), [100]);
    assert_eq!(journal.load().unwrap(), session.dom);
}

#[test]
fn snapshot_full_replay_matches_when_every_snapshot_is_damaged() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let mut session = Session::new("s");
    session.grow_to(250);
    let mut journal = Journal::create(&dir, "s", every(100)).unwrap();
    write(&mut journal, &session);
    drop(journal);
    for snap in snapshot_files(&dir) {
        flip_byte(&snap, file_len(&snap) - 1);
    }
    let (journal, _) = Journal::open(&dir, every(100)).unwrap();
    assert_eq!(journal.load().unwrap(), session.dom);
}

#[test]
fn leftover_temp_snapshot_from_a_crash_is_ignored_and_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let mut session = Session::new("s");
    session.grow_to(150);
    let mut journal = Journal::create(&dir, "s", every(100)).unwrap();
    write(&mut journal, &session);
    drop(journal);
    let tmp_snap = dir.join("snap-00000000000000000150.loas.tmp");
    fs::File::create(&tmp_snap).unwrap().write_all(b"LOASNAP\0 half").unwrap();
    let (journal, _) = Journal::open(&dir, every(100)).unwrap();
    assert!(!tmp_snap.exists());
    assert_eq!(journal.snapshot_seqs().collect::<Vec<_>>(), [100]);
    assert_eq!(journal.load().unwrap(), session.dom);
}

#[test]
fn snapshot_needs_the_dom_at_the_last_patch() {
    let tmp = tempfile::tempdir().unwrap();
    let mut session = Session::new("s");
    session.grow_to(10);
    let mut journal = Journal::create(&tmp.path().join("s"), "s", no_snapshots()).unwrap();
    write(&mut journal, &session);
    assert!(matches!(journal.write_snapshot(&session.dom_at(9)), Err(Error::DomMismatch { dom: 9, journal: 10 })));
    journal.write_snapshot(&session.dom).unwrap();
    assert_eq!(journal.snapshot_seqs().collect::<Vec<_>>(), [10]);
}

#[test]
fn text_and_attribute_values_survive_the_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("s");
    let mut session = Session::new("s");
    let ext = session.commit(Draft::insert(
        session.root,
        ElementKind::Ext,
        [
            (attr::HARNESS, Value::from("claude-code")),
            ("x-neg", Value::Int(-42)),
            ("x-nan-free", Value::Float(f64::MIN_POSITIVE)),
            ("x-raw", Value::Bytes((0..=255).collect())),
            ("x-nested", Value::List(vec![Value::List(vec![Value::from("deep")]), Value::Bool(false)])),
        ],
        "naïve \u{1F600} text",
    ));
    session.commit(Draft::insert_at(session.root, 0, ElementKind::Ext, [(attr::HARNESS, Value::from("x"))], ""));
    session.commit(Draft::clear(ext, "x-neg"));
    let mut journal = Journal::create(&dir, "s", every(4)).unwrap();
    write(&mut journal, &session);
    drop(journal);
    let (journal, _) = Journal::open(&dir, every(4)).unwrap();
    let dom = journal.load().unwrap();
    assert_eq!(dom, session.dom);
    assert_eq!(dom.children(session.root).next().map(|n| n.kind), Some(ElementKind::Ext));
    assert_eq!(dom.next_id(), NodeId(5));
}
