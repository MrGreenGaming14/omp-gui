#![allow(dead_code)]

use loa_bus::{Authorize, Journal, JournalOptions, NodeKey, Persist, SessionWriter, WriterConfig};
use loa_core::testkit::{AllowAll, DomFixture, MemoryPersist};
use loa_core::{Dom, Draft, ElementKind, NodeId, Patch, Role, Value, attrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, mem};

pub const OWNER: NodeKey = NodeKey([1; 32]);
pub const PEER: NodeKey = NodeKey([2; 32]);

/// A live session of three patches (session, turn, empty assistant message).
pub struct Fixture {
    pub writer: SessionWriter,
    /// Every patch persisted after the first three.
    pub log: MemoryPersist,
    pub session: NodeId,
    pub turn: NodeId,
    pub message: NodeId,
}

impl Fixture {
    pub fn seqs(&self) -> Vec<u64> {
        self.log.patches.lock().unwrap().iter().map(|p| p.seq).collect()
    }

    pub fn text(&self) -> String {
        self.writer.with_dom(|dom| dom.get(self.message).unwrap().text.clone())
    }
}

/// The fixture session owned by [`OWNER`] as seen from `local`.
pub fn fixture_with(local: NodeKey, log: MemoryPersist, authorize: Arc<dyn Authorize>) -> Fixture {
    let mut built = DomFixture::session();
    let message = built.turn().message(Role::Assistant, "").last;
    let dom = built.build();
    let turn = dom.get(message).unwrap().parent;
    let persist: Box<dyn Persist> = Box::new(log.clone());
    let config = WriterConfig { session: "s1".into(), owner: OWNER, local, dom, persist, authorize };
    Fixture { writer: SessionWriter::new(config), log, session: built.session, turn, message }
}

pub fn fixture() -> Fixture {
    fixture_with(OWNER, MemoryPersist::default(), Arc::new(AllowAll))
}

/// `n` patches of every kind from an empty DOM, and the DOM after them.
pub fn history(id: &str, n: usize) -> (Vec<Patch>, Dom) {
    let mut dom = Dom::new();
    let mut out = Vec::new();
    let commit = |dom: &mut Dom, out: &mut Vec<Patch>, d: Draft| {
        let p = dom.commit(d, 1_700_000_000_000 + out.len() as i64).unwrap();
        out.push(p.clone());
        p.node
    };
    let root = attrs! { ID: id, HARNESS: "omp", NODE: "k", CREATED: 1i64 };
    let root = commit(&mut dom, &mut out, Draft::insert(NodeId::DOCUMENT, ElementKind::Session, root, ""));
    let turn = commit(&mut dom, &mut out, Draft::insert(root, ElementKind::Turn, attrs! {}, ""));
    let raw = attrs! { HARNESS: "x", "x-neg": -42i64, "x-raw": Value::Bytes((0..=255).collect()) };
    commit(&mut dom, &mut out, Draft::insert(root, ElementKind::Ext, raw, "naïve \u{1F600}"));
    for i in 0i64.. {
        let ext = attrs! { HARNESS: "omp", "x-f": 0.5, "x-l": Value::List(vec![Value::Bool(true), Value::from(i)]) };
        let msg = attrs! { ROLE: Role::Assistant };
        let drafts = |msg: NodeId, ext: NodeId| match i % 4 {
            0 => Draft::append(msg, format!("tok{i} ")),
            1 => Draft::set(ext, "x-n", i),
            2 => Draft::insert_at(root, 0, ElementKind::Ext, attrs! { HARNESS: "x" }, ""),
            _ => Draft::remove(ext),
        };
        let m = commit(&mut dom, &mut out, Draft::insert(turn, ElementKind::Message, msg, ""));
        let e = commit(&mut dom, &mut out, Draft::insert(root, ElementKind::Ext, ext, "raw"));
        commit(&mut dom, &mut out, drafts(m, e));
        if out.len() >= n {
            break;
        }
    }
    out.truncate(n);
    let mut dom = Dom::new();
    out.iter().for_each(|p| dom.apply(p).unwrap());
    (out, dom)
}

/// The DOM right after `patches[..seq]`.
pub fn dom_at(patches: &[Patch], seq: u64) -> Dom {
    let mut dom = Dom::new();
    patches[..seq as usize].iter().for_each(|p| dom.apply(p).unwrap());
    dom
}

/// Appends `patches` after the journal's last one as a writer would, syncs,
/// and returns the offset of each appended record.
pub fn write(journal: &mut Journal, patches: &[Patch]) -> Vec<u64> {
    let mut dom = dom_at(patches, journal.last_seq());
    let file = journal_file(journal.dir());
    let mut offsets = Vec::new();
    for p in &patches[journal.last_seq() as usize..] {
        offsets.push(file_len(&file));
        dom.apply(p).unwrap();
        journal.append_applied(p, &dom).unwrap();
    }
    journal.sync().unwrap();
    offsets
}

/// A new journal `dir/id` holding `patches`, closed again.
pub fn written(dir: &Path, id: &str, patches: &[Patch], options: JournalOptions) -> (PathBuf, Vec<u64>) {
    let path = dir.join(id);
    let mut journal = Journal::create(&path, id, options).unwrap();
    let offsets = write(&mut journal, patches);
    mem::drop(journal);
    (path, offsets)
}

pub fn every(n: u64) -> JournalOptions {
    JournalOptions { snapshot_every: n, keep_snapshots: 2 }
}

pub fn flip_byte(path: &Path, offset: u64) {
    let mut bytes = fs::read(path).unwrap();
    bytes[offset as usize] ^= 0x5a;
    fs::write(path, bytes).unwrap();
}

pub fn journal_file(dir: &Path) -> PathBuf {
    dir.join(loa_bus::JOURNAL_FILE)
}

pub fn file_len(path: &Path) -> u64 {
    fs::metadata(path).unwrap().len()
}

/// Snapshot files in `dir`, oldest first.
pub fn snapshot_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "loas"))
        .collect();
    files.sort();
    files
}
