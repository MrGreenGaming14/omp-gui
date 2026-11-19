mod common;

use common::{Session, snapshot_files};
use loa_bus::{Authorize, Denied, NodeKey, SessionId, SessionWriter, WriterConfig};
use loa_core::{Dom, Draft, Op, Patch, PatchSink};
use loa_journal::{JournalOptions, JournalPersist, JournalWriter, Store, SyncPolicy};
use std::sync::Arc;

struct Nobody;

impl Authorize for Nobody {
    fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
        Err(Denied("local only".into()))
    }
}

fn draft(patch: &Patch) -> Draft {
    match patch.op.clone() {
        Op::Insert { parent, index, kind, attrs, text } => Draft::Insert { parent, index, kind, attrs, text },
        Op::Set { name, value } => Draft::Set { node: patch.node, name, value },
        Op::Append { text } => Draft::Append { node: patch.node, text },
        Op::Remove => Draft::Remove { node: patch.node },
    }
}

#[test]
fn a_session_writer_persists_through_the_journal_and_snapshots_from_its_own_dom() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path(), JournalOptions { snapshot_every: 5, keep_snapshots: 2 }).unwrap();
    let journal = Arc::new(JournalWriter::new(store.create("s1").unwrap(), SyncPolicy::Manual));
    let key = NodeKey([7; 32]);
    let writer = SessionWriter::new(WriterConfig {
        session: "s1".into(),
        owner: key,
        local: key,
        dom: Dom::new(),
        persist: Box::new(JournalPersist(Arc::clone(&journal))),
        authorize: Arc::new(Nobody),
    });
    let mut expected = Session::new("s1");
    expected.grow_to(23);
    for patch in &expected.patches {
        writer.commit_at(draft(patch), patch.ts).unwrap();
    }
    assert_eq!(journal.last_seq(), 23);
    // Snapshots at 5, 10, 15, 20; two kept. Each synced the journal.
    let snaps = snapshot_files(&dir.path().join("s1"));
    assert_eq!(snaps.len(), 2, "{snaps:?}");
    assert_eq!(journal.pending(), 3);
    assert_eq!(journal.load_at(12).unwrap(), expected.dom_at(12));
    drop(writer);
    drop(journal);

    let (reopened, truncation) = store.open("s1").unwrap();
    assert!(truncation.is_none());
    assert_eq!(reopened.load().unwrap(), expected.dom);
    assert_eq!(reopened.snapshot_seqs().collect::<Vec<_>>(), vec![15, 20]);
}
