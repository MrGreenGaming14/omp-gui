//! The journal-backed home of every session on this node: live writers on
//! the bus persisted through their journals, plus the [`SessionStore`] port
//! that fork, rewind, compaction and subagents create sessions through.

use loa_util::LockExt;
use crate::{Error, Result, SessionStore};
use async_trait::async_trait;
use loa_contracts::harness::Adapter;
use loa_bus::{Authorize, Bus, NodeKey, SessionWriter, WriterConfig};
use loa_core::{Dom, Draft, ElementKind, NodeId, Op, PatchSink, Value, attr};
use loa_journal::{Journal, JournalPersist, JournalWriter, Store, SyncPolicy};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// One live session: the writer every change goes through and the journal it
/// persists to, for reads of earlier states and forks.
#[derive(Clone)]
pub struct Live {
    pub writer: Arc<SessionWriter>,
    pub journal: Arc<JournalWriter>,
}

/// The sessions under one journal [`Store`], opened as writers on one [`Bus`].
pub struct Library {
    store: Store,
    bus: Arc<Bus>,
    local: NodeKey,
    authorize: Arc<dyn Authorize>,
    sync: SyncPolicy,
    journals: Mutex<HashMap<String, Arc<JournalWriter>>>,
}

impl Library {
    /// `local` owns every session this library creates or opens; peer
    /// requests pass `authorize`.
    pub fn new(store: Store, bus: Arc<Bus>, local: NodeKey, authorize: Arc<dyn Authorize>) -> Self {
        Self { store, bus, local, authorize, sync: SyncPolicy::default(), journals: Mutex::default() }
    }

    pub fn with_sync(mut self, sync: SyncPolicy) -> Self {
        self.sync = sync;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn bus(&self) -> &Arc<Bus> {
        &self.bus
    }

    /// Creates an empty session and opens its writer.
    pub fn create(&self, id: &str) -> Result<Live> {
        let journal = self.store.create(id)?;
        self.attach(id, journal, Dom::new())
    }

    /// The live session `id`, opening it from its journal when it isn't live.
    pub fn open(&self, id: &str) -> Result<Live> {
        if let Some(live) = self.live(id) {
            return Ok(live);
        }
        let (journal, _truncation) = self.store.open(id)?;
        let dom = journal.load()?;
        self.attach(id, journal, dom)
    }

    fn attach(&self, id: &str, journal: Journal, dom: Dom) -> Result<Live> {
        let journal = Arc::new(JournalWriter::new(journal, self.sync));
        let writer = self.bus.open(WriterConfig {
            session: id.into(),
            owner: self.local,
            local: self.local,
            dom,
            persist: Box::new(JournalPersist(Arc::clone(&journal))),
            authorize: Arc::clone(&self.authorize),
        })?;
        self.journals.locked().insert(id.to_owned(), Arc::clone(&journal));
        Ok(Live { writer, journal })
    }

    /// The session if it is live.
    pub fn live(&self, id: &str) -> Option<Live> {
        let journal = self.journals.locked().get(id).cloned()?;
        let writer = self.bus.get(id)?;
        Some(Live { writer, journal })
    }

    /// Closes a live session: its writer refuses commits, its journal is
    /// synced and released (even while harness tasks still hold the writer),
    /// so it can be reopened or deleted. Returns whether it was live.
    pub fn close(&self, id: &str) -> Result<bool> {
        let journal = self.journals.locked().remove(id);
        let writer = self.bus.close(id);
        if let Some(journal) = &journal {
            journal.close()?;
        }
        Ok(journal.is_some() || writer.is_some())
    }

    /// Closes and deletes a session. Its forks keep working.
    pub fn delete(&self, id: &str) -> Result<()> {
        self.close(id)?;
        Ok(self.store.delete(id)?)
    }

    /// The current DOM of a session, live or not.
    pub fn load(&self, id: &str) -> Result<Dom> {
        if let Some(live) = self.live(id) {
            return Ok(live.writer.with_dom(Dom::clone));
        }
        let (journal, _) = self.store.open(id)?;
        Ok(journal.load()?)
    }

    /// The session's DOM right after patch `seq`.
    pub fn load_at(&self, id: &str, seq: u64) -> Result<Dom> {
        if let Some(live) = self.live(id) {
            return Ok(live.journal.load_at(seq)?);
        }
        let (journal, _) = self.store.open(id)?;
        Ok(journal.load_at(seq)?)
    }

    /// Sequence of the patch that inserted each `<turn>` directly under the
    /// session root, in journal order. Turns removed later are left out.
    pub fn turn_seqs(&self, id: &str) -> Result<Vec<(NodeId, u64)>> {
        let dom = self.load(id)?;
        let Some(root) = dom.root().map(|r| r.id) else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        self.each_patch(id, |patch| {
            if let Op::Insert { parent, kind: ElementKind::Turn, .. } = &patch.op
                && *parent == root
                && dom.contains(patch.node)
            {
                out.push((patch.node, patch.seq));
            }
        })?;
        Ok(out)
    }

    /// Sequence of the patch that inserted `node`.
    pub fn inserted_at(&self, id: &str, node: NodeId) -> Result<Option<u64>> {
        let mut found = None;
        self.each_patch(id, |patch| {
            if found.is_none() && patch.node == node && matches!(patch.op, Op::Insert { .. }) {
                found = Some(patch.seq);
            }
        })?;
        Ok(found)
    }

    fn each_patch(&self, id: &str, mut f: impl FnMut(&loa_core::Patch)) -> Result<()> {
        let patches = match self.live(id) {
            Some(live) => live.journal.patches(1)?,
            None => self.store.open(id)?.0.patches(1)?,
        };
        for patch in patches {
            f(&patch?);
        }
        Ok(())
    }

    /// The DOM to rewind `id` to so that the turn holding `node` (the turn
    /// itself, or a message in it, such as the user prompt clicked in the
    /// chat) and everything after it is undone: the state right before the
    /// patch that inserted that turn.
    pub fn before_turn(&self, id: &str, node: NodeId) -> Result<Dom> {
        let dom = self.load(id)?;
        let turn = std::iter::once(node)
            .chain(dom.ancestors(node).map(|n| n.id))
            .find(|n| dom.get(*n).is_some_and(|n| n.kind == ElementKind::Turn))
            .ok_or(Error::InvalidSequence)?;
        let seq = self.inserted_at(id, turn)?.ok_or(Error::InvalidSequence)?;
        self.load_at(id, seq - 1)
    }

    /// The DOM to fork `id` from so that `turn` is the last turn kept: the
    /// state right before the next turn started, or the current one.
    pub fn through_turn(&self, id: &str, turn: NodeId) -> Result<Dom> {
        let turns = self.turn_seqs(id)?;
        let at = turns.iter().position(|(node, _)| *node == turn).ok_or(Error::InvalidSequence)?;
        match turns.get(at + 1) {
            Some((_, next)) => self.load_at(id, next - 1),
            None => self.load(id),
        }
    }

    /// Reads a native harness session into a new session `id` through
    /// `adapter`, keeping its timestamps. The imported root gets `id`, the
    /// local `node` and, when the harness named it differently, `native_id`.
    pub fn import(&self, adapter: &dyn Adapter, native: &Path, id: &str, node: &str) -> Result<Live> {
        let live = self.create(id)?;
        let imported = adapter.import(native, live.writer.as_ref()).map_err(Error::from).and_then(|()| {
            let (root, native_id, owner) = live.writer.with_dom(|dom| {
                let root = dom.root();
                (
                    root.map(|r| r.id),
                    root.and_then(|r| r.str_attr(attr::ID)).map(str::to_owned),
                    root.and_then(|r| r.str_attr(attr::NODE)).map(str::to_owned),
                )
            });
            let root = root.ok_or(Error::NoSession)?;
            if let Some(native_id) = native_id.filter(|n| n != id) {
                if live.writer.with_dom(|dom| dom.get(root).and_then(|r| r.attr(attr::NATIVE_ID)).is_none()) {
                    live.writer.commit(Draft::set(root, attr::NATIVE_ID, native_id))?;
                }
                live.writer.commit(Draft::set(root, attr::ID, id))?;
            }
            if owner.as_deref() != Some(node) {
                live.writer.commit(Draft::set(root, attr::NODE, node))?;
            }
            Ok(())
        });
        if let Err(error) = imported {
            // Leave no half imported session behind.
            let _ = self.delete(id);
            return Err(error);
        }
        live.journal.sync()?;
        Ok(live)
    }

    /// Imports a subagent's native session file as a child of `parent`,
    /// linked at the patch that inserted its `<job>`. The child id derives
    /// from the file, so opening the same job twice reuses it; a file that
    /// changed since (the child kept running) is imported afresh.
    pub fn import_child(&self, adapter: &dyn Adapter, parent: &str, job: NodeId, native: &Path, node: &str) -> Result<String> {
        let id = child_id(native);
        let modified = std::fs::metadata(native)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let existing = self.store.list()?.iter().any(|l| matches!(l, loa_journal::Listing::Session(i) if i.id == id));
        if existing {
            let fresh = self.load(&id)?.root().and_then(|r| r.int_attr(SOURCE_MODIFIED)) == Some(modified);
            if fresh {
                return Ok(id);
            }
            self.delete(&id)?;
        }
        let forked_at = self.inserted_at(parent, job)?.unwrap_or(0);
        let live = self.import(adapter, native, &id, node)?;
        let root = live.writer.with_dom(|dom| dom.root().map(|r| r.id)).ok_or(Error::NoSession)?;
        live.writer.commit(Draft::set(root, attr::PARENT_SESSION, parent))?;
        live.writer.commit(Draft::set(root, attr::FORKED_AT, i64::try_from(forked_at).unwrap_or(i64::MAX)))?;
        live.writer.commit(Draft::set(root, SOURCE_MODIFIED, Value::Int(modified)))?;
        live.journal.sync()?;
        Ok(id)
    }

    /// Writes session `id` as a native session of `adapter`'s harness.
    pub fn export(&self, adapter: &dyn Adapter, id: &str, out: &Path) -> Result<()> {
        let dom = self.load(id)?;
        Ok(adapter.export(&dom, out)?)
    }

    /// Writes session `id` as debugging XML.
    pub fn export_xml(&self, id: &str, out: &Path) -> Result<()> {
        let dom = self.load(id)?;
        std::fs::write(out, loa_core::xml::to_string(&dom)).map_err(|e| Error::Port(format!("{}: {e}", out.display())))
    }
}

/// Root attribute of an imported child: modification time (unix ms) of the
/// native file it was read from.
const SOURCE_MODIFIED: &str = "x-loa-source-modified";

/// Stable session id for a native child session file.
fn child_id(path: &Path) -> String {
    // FNV-1a: stable across runs and platforms, unlike `DefaultHasher`.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("child-{hash:016x}")
}

#[async_trait]
impl SessionStore for Library {
    async fn fork(&self, source_id: &str, seq: u64, new_id: &str) -> Result<(Dom, Arc<dyn PatchSink>)> {
        let fork = match self.live(source_id) {
            Some(live) => live.journal.fork(seq, new_id)?,
            None => self.store.open(source_id)?.0.fork(seq, new_id)?,
        };
        let dom = fork.load()?;
        let live = self.attach(new_id, fork, dom.clone())?;
        Ok((dom, live.writer))
    }

    async fn fresh(&self, new_id: &str) -> Result<Arc<dyn PatchSink>> {
        Ok(self.create(new_id)?.writer)
    }
}
