#![allow(dead_code)]

use loa_core::{Dom, Draft, ElementKind, NodeId, Patch, Role, Value, attr};
use loa_journal::{JOURNAL_FILE, Journal};
use std::fs;
use std::path::Path;

/// A session being written: the writer's DOM and every committed patch.
#[derive(Clone)]
pub struct Session {
    pub dom: Dom,
    pub patches: Vec<Patch>,
    pub root: NodeId,
    pub turn: NodeId,
}

impl Session {
    pub fn new(id: &str) -> Self {
        let mut s = Session { dom: Dom::new(), patches: Vec::new(), root: NodeId::DOCUMENT, turn: NodeId::DOCUMENT };
        s.root = s.commit(Draft::insert(
            NodeId::DOCUMENT,
            ElementKind::Session,
            [
                (attr::ID, Value::from(id)),
                (attr::HARNESS, "omp".into()),
                (attr::NODE, "node-key".into()),
                (attr::CREATED, Value::Int(1_700_000_000_000)),
            ],
            "",
        ));
        s.turn = s.commit(Draft::insert(s.root, ElementKind::Turn, [(attr::STATUS, Value::from("running"))], ""));
        s
    }

    pub fn commit(&mut self, draft: Draft) -> NodeId {
        let ts = 1_700_000_000_000 + self.patches.len() as i64;
        let patch = self.dom.commit(draft, ts).unwrap();
        self.patches.push(patch.clone());
        patch.node
    }

    /// Adds patches of every kind until `count` patches exist: messages with
    /// streamed tokens, attribute sets, positional inserts and removals.
    pub fn grow_to(&mut self, count: usize) {
        let mut i = 0usize;
        while self.patches.len() < count {
            let msg = self.commit(Draft::insert(
                self.turn,
                ElementKind::Message,
                [(attr::ROLE, Value::from(Role::Assistant)), (attr::STATUS, "streaming".into())],
                "",
            ));
            for t in 0..5 {
                if self.patches.len() >= count {
                    return;
                }
                self.commit(Draft::append(msg, format!("tok{i}-{t} ")));
            }
            if self.patches.len() >= count {
                return;
            }
            self.commit(Draft::set(msg, attr::STATUS, "done"));
            if self.patches.len() >= count {
                return;
            }
            let ext = self.commit(Draft::insert_at(
                self.root,
                0,
                ElementKind::Ext,
                [(attr::HARNESS, Value::from("omp")), ("x-n", Value::Int(i as i64)), ("x-f", Value::Float(0.5))],
                "raw",
            ));
            if self.patches.len() >= count {
                return;
            }
            if i.is_multiple_of(2) {
                self.commit(Draft::remove(ext));
            } else {
                self.commit(Draft::set(ext, "x-list", Value::List(vec![Value::Bytes(vec![1, 2]), Value::Bool(true)])));
            }
            i += 1;
        }
    }

    /// The DOM right after patch `seq`.
    pub fn dom_at(&self, seq: u64) -> Dom {
        let mut dom = Dom::new();
        for p in &self.patches[..seq as usize] {
            dom.apply(p).unwrap();
        }
        dom
    }
}

/// Appends every patch of `session` from `journal.last_seq() + 1` as the
/// writer would, and returns the journal file length before each appended
/// patch (the record offsets).
pub fn write(journal: &mut Journal, session: &Session) -> Vec<u64> {
    let mut dom = session.dom_at(journal.last_seq());
    let mut offsets = Vec::new();
    let file = journal.dir().join(JOURNAL_FILE);
    for p in &session.patches[journal.last_seq() as usize..] {
        offsets.push(fs::metadata(&file).unwrap().len());
        dom.apply(p).unwrap();
        journal.append_applied(p, &dom).unwrap();
    }
    journal.sync().unwrap();
    offsets
}

pub fn flip_byte(path: &Path, offset: u64) {
    let mut bytes = fs::read(path).unwrap();
    bytes[offset as usize] ^= 0x5a;
    fs::write(path, bytes).unwrap();
}

pub fn journal_file(dir: &Path) -> std::path::PathBuf {
    dir.join(JOURNAL_FILE)
}

pub fn file_len(path: &Path) -> u64 {
    fs::metadata(path).unwrap().len()
}

/// Snapshot files in `dir`, oldest first.
pub fn snapshot_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "loas"))
        .collect();
    files.sort();
    files
}
