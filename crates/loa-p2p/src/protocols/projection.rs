//! Owner-ordered patch sharing: a peer sees an independently numbered
//! projection of the owner's journal, never the patches its grant hides.

use crate::{NodeError, Result};
use loa_bus::{Journal, NodeKey, SessionWriter, Store};
use loa_core::{Dom, Draft, ElementKind, NodeId, Op, Patch, Value, attr};
use loa_trust::Action;
use std::collections::HashMap;
use std::sync::Arc;

/// Port: whether `peer` may take `action` on the session or on `node` in it.
pub trait Access: Send + Sync {
    fn allows(&self, peer: NodeKey, dom: &Dom, action: Action, node: Option<NodeId>) -> bool;
}

/// A source patch and what it changed in the projection; skipped ones still advance `source_seq`.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub source_seq: u64,
    pub patch: Option<Patch>,
}

/// Filters the source journal in order, from a [`Projection::seeded`] start.
pub struct Projection {
    peer: NodeKey,
    source: Dom,
    visible: Dom,
    ids: HashMap<NodeId, NodeId>,
}

impl Projection {
    pub fn new(peer: NodeKey) -> Self {
        Self { peer, source: Dom::new(), visible: Dom::new(), ids: HashMap::new() }
    }

    pub fn source_seq(&self) -> u64 { self.source.seq() }
    pub fn visible_dom(&self) -> &Dom { &self.visible }

    /// The owner's id of the node the peer sees as `visible`.
    pub fn source_node(&self, visible: NodeId) -> Option<NodeId> {
        self.ids.iter().find(|(_, v)| **v == visible).map(|(s, _)| *s)
    }

    /// A draft against the visible DOM in the owner's ids; `None` when it names a hidden node.
    pub fn source_draft(&self, draft: Draft) -> Option<Draft> {
        let source = |visible: NodeId| self.source_node(visible);
        Some(match draft {
            Draft::Insert { parent, index, kind, attrs, text } => {
                let parent_source = source(parent)?;
                // The peer counts only the children it sees; place the new
                // node after the source child it would follow.
                let index = match index {
                    None => None,
                    Some(0) => Some(0),
                    Some(i) => {
                        let visible_prev = *self.visible.get(parent)?.children.get(i as usize - 1)?;
                        let source_prev = source(visible_prev)?;
                        let position = self.source.get(parent_source)?.children.iter().position(|c| *c == source_prev)?;
                        Some(u32::try_from(position + 1).ok()?)
                    }
                };
                Draft::Insert { parent: parent_source, index, kind, attrs, text }
            }
            Draft::Set { node, name, value } => Draft::Set { node: source(node)?, name, value },
            Draft::Append { node, text } => Draft::Append { node: source(node)?, text },
            Draft::Remove { node } => Draft::Remove { node: source(node)? },
        })
    }

    /// A projection of the owner's current DOM and the patches that build the peer's view.
    pub fn seeded(peer: NodeKey, source: &Dom, access: &dyn Access) -> Result<(Self, Vec<Patch>)> {
        let mut projection = Self { peer, source: source.clone(), visible: Dom::new(), ids: HashMap::new() };
        let whole = access.allows(peer, source, Action::View, None);
        let mut patches = Vec::new();
        let ts = source.root().and_then(|root| root.int_attr(attr::CREATED)).unwrap_or_default();
        for node in source.iter() {
            let draft = if node.kind == ElementKind::Session {
                let attrs = node.attrs.iter()
                    .filter(|(name, _)| root_attr_visible(name, whole))
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                Draft::Insert { parent: NodeId::DOCUMENT, index: None, kind: node.kind, attrs, text: String::new() }
            } else {
                let Some(&parent) = projection.ids.get(&node.parent) else { continue };
                if !access.allows(peer, source, Action::View, Some(node.id)) { continue; }
                Draft::Insert { parent, index: None, kind: node.kind, attrs: node.attrs.clone(), text: node.text.clone() }
            };
            let patch = projection.visible.commit(draft, ts)?;
            projection.ids.insert(node.id, patch.node);
            patches.push(patch);
        }
        Ok((projection, patches))
    }

    pub fn accept(&mut self, source_patch: &Patch, access: &dyn Access) -> Result<Update> {
        let removed_ids = if matches!(source_patch.op, Op::Remove) {
            self.source.descendants(source_patch.node).map(|node| node.id).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let remove_allowed = matches!(source_patch.op, Op::Remove)
            && access.allows(self.peer, &self.source, Action::View, Some(source_patch.node));
        self.source.apply(source_patch)?;
        let old_visible = self.ids.contains_key(&source_patch.node);
        let allowed = remove_allowed
            || access.allows(self.peer, &self.source, Action::View, Some(source_patch.node));
        let update = match &source_patch.op {
            Op::Insert { parent, kind, attrs, text, .. } if allowed || *kind == ElementKind::Session => {
                let (parent, index, attrs, text) = if *kind == ElementKind::Session {
                    // A turn-only grant must not reveal project paths, digest,
                    // pre-existing context, or arbitrary root extension data.
                    let whole = access.allows(self.peer, &self.source, Action::View, None);
                    let safe = attrs.iter()
                        .filter(|(name, _)| root_attr_visible(name, whole))
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect();
                    (NodeId::DOCUMENT, None, safe, String::new())
                } else {
                    let Some(&visible_parent) = self.ids.get(parent) else {
                        return Ok(Update { source_seq: source_patch.seq, patch: None });
                    };
                    let preceding = self.source.children(*parent)
                        .take_while(|child| child.id != source_patch.node)
                        .filter(|child| self.ids.contains_key(&child.id))
                        .count() as u32;
                    (visible_parent, Some(preceding), attrs.clone(), text.clone())
                };
                let patch = self.visible.commit(Draft::Insert { parent, index, kind: *kind, attrs, text }, source_patch.ts)?;
                self.ids.insert(source_patch.node, patch.node);
                Some(patch)
            }
            Op::Set { name, value } if old_visible && self.source.root().is_some_and(|root| root.id == source_patch.node) => {
                let whole = access.allows(self.peer, &self.source, Action::View, None);
                if root_attr_visible(name, whole) {
                    Some(self.visible.commit(Draft::Set { node: self.ids[&source_patch.node], name: name.clone(), value: value.clone() }, source_patch.ts)?)
                } else {
                    None
                }
            }
            Op::Set { name, value } if allowed && old_visible => {
                Some(self.visible.commit(Draft::Set { node: self.ids[&source_patch.node], name: name.clone(), value: value.clone() }, source_patch.ts)?)
            }
            Op::Append { text } if allowed && old_visible => {
                Some(self.visible.commit(Draft::Append { node: self.ids[&source_patch.node], text: text.clone() }, source_patch.ts)?)
            }
            Op::Remove if old_visible => {
                let node = self.ids[&source_patch.node];
                for descendant in removed_ids { self.ids.remove(&descendant); }
                Some(self.visible.commit(Draft::Remove { node }, source_patch.ts)?)
            }
            _ => None,
        };
        Ok(Update { source_seq: source_patch.seq, patch: update })
    }
}

/// Root attributes a peer sees: provenance always, the title with a whole-session grant.
fn root_attr_visible(name: &str, whole_session: bool) -> bool {
    matches!(name, attr::ID | attr::SCHEMA | attr::HARNESS | attr::NODE | attr::CREATED)
        || (whole_session && name == attr::TITLE)
}

/// A read-only copy of a peer's session: its seed, then every [`Update`] in order, gaps refused.
pub struct Mirror {
    writer: Arc<SessionWriter>,
    source_seq: u64,
}

impl Mirror {
    pub fn new(writer: Arc<SessionWriter>, source_seq: u64) -> Result<Self> {
        if writer.is_writer() { return Err(NodeError::NotOwner); }
        Ok(Self { writer, source_seq })
    }

    /// A mirror on an empty `writer` built from the owner's seed patches.
    pub fn seeded(writer: Arc<SessionWriter>, source_seq: u64, patches: Vec<Patch>) -> Result<Self> {
        if writer.seq() != 0 {
            return Err(NodeError::Sequence { last: writer.seq(), got: 0 });
        }
        let mirror = Self::new(writer, source_seq)?;
        for patch in patches {
            mirror.writer.replicate(patch)?;
        }
        Ok(mirror)
    }

    pub fn source_seq(&self) -> u64 { self.source_seq }
    pub fn writer(&self) -> &Arc<SessionWriter> { &self.writer }

    pub fn receive(&mut self, update: Update) -> Result<()> {
        if update.source_seq != self.source_seq + 1 {
            return Err(NodeError::Sequence { last: self.source_seq, got: update.source_seq });
        }
        if let Some(patch) = update.patch { self.writer.replicate(patch)?; }
        self.source_seq = update.source_seq;
        Ok(())
    }
}

/// Copies mirrored `patches` up to `at_seq` into a new journal this node owns, re-rooted as a fork.
pub fn fork_mirror_to_own_node(store: &Store, patches: &[Patch], at_seq: u64, new_id: &str, local_node: &str) -> Result<Journal> {
    let built = (|| {
        let mut journal = store.create(new_id)?;
        let mut dom = Dom::new();
        for patch in patches.iter().take_while(|patch| patch.seq <= at_seq) {
            dom.apply(patch)?;
            journal.append(patch)?;
        }
        if dom.seq() != at_seq {
            return Err(NodeError::Sequence { last: dom.seq(), got: at_seq });
        }
        let root = dom.root().ok_or_else(|| NodeError::Unavailable("session has no root".into()))?;
        let (root, source) = (root.id, root.str_attr(attr::ID).unwrap_or_default().to_owned());
        let ts = loa_core::now_ms();
        let drafts = [
            Draft::set(root, attr::ID, new_id),
            Draft::set(root, attr::PARENT_SESSION, source.as_str()),
            Draft::set(root, attr::FORKED_AT, Value::Int(i64::try_from(at_seq).unwrap_or(i64::MAX))),
            Draft::set(root, attr::NODE, local_node),
        ];
        for draft in drafts {
            let patch = dom.commit(draft, ts)?;
            journal.append_applied(&patch, &dom)?;
        }
        journal.write_snapshot(&dom)?;
        journal.sync()?;
        Ok(journal)
    })();
    if built.is_err() {
        // Leave no half made session behind; a store that refuses the id
        // (already present, invalid) is left untouched by `create` itself.
        if !matches!(built, Err(NodeError::Journal(loa_bus::JournalError::InvalidSessionId(_) | loa_bus::JournalError::Exists(_)))) {
            let _ = store.delete(new_id);
        }
    }
    built
}


#[cfg(test)]
mod tests {
    use super::*;

    /// View on the whole session (`None`) or on one turn's subtree.
    struct View(Option<NodeId>);

    impl Access for View {
        fn allows(&self, _: NodeKey, dom: &Dom, action: Action, node: Option<NodeId>) -> bool {
            action == Action::View
                && self.0.is_none_or(|turn| node.is_some_and(|id| id == turn || dom.ancestors(id).any(|a| a.id == turn)))
        }
    }

    fn shape(dom: &Dom) -> Vec<String> { dom.iter().map(|n| format!("{:?} {} {:?}", n.kind, n.text, n.attrs)).collect() }

    #[test]
    fn seeded_projection_matches_replay_and_hides_other_turns() {
        let (mut source, mut patches) = (Dom::new(), Vec::new());
        let mut add = |dom: &mut Dom, draft| {
            let patch = dom.commit(draft, 1).unwrap();
            patches.push(patch.clone());
            patch
        };
        let none = || std::iter::empty::<(&str, Value)>();
        let root = add(&mut source, Draft::insert(NodeId::DOCUMENT, ElementKind::Session, [
            (attr::ID, Value::from("s")), (attr::NODE, Value::from("owner")), (attr::CREATED, Value::Int(1)), (attr::PROJECT, Value::from("private")),
        ], ""));
        add(&mut source, Draft::set(root.node, attr::TITLE, "Plan"));
        let hidden = add(&mut source, Draft::insert(root.node, ElementKind::Turn, none(), ""));
        let shown = add(&mut source, Draft::insert(root.node, ElementKind::Turn, none(), ""));
        let message = add(&mut source, Draft::insert(shown.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], "hel"));
        add(&mut source, Draft::append(message.node, "lo"));
        add(&mut source, Draft::insert(hidden.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], "secret"));
        let peer = NodeKey([1; 32]);
        for access in [View(Some(shown.node)), View(None)] {
            let mut replayed = Projection::new(peer);
            let updates: Vec<_> = patches.iter().map(|p| replayed.accept(p, &access).unwrap()).collect();
            assert_eq!(updates.last().unwrap().source_seq, source.seq());
            let (seeded, seed) = Projection::seeded(peer, &source, &access).unwrap();
            assert_eq!(shape(seeded.visible_dom()), shape(replayed.visible_dom()));
            let mut mirror = Dom::new();
            seed.iter().for_each(|p| mirror.apply(p).unwrap());
            assert_eq!(shape(&mirror), shape(seeded.visible_dom()));
            let root = mirror.root().unwrap();
            assert!(!root.attrs.contains_key(attr::PROJECT));
            assert_eq!(root.attrs.contains_key(attr::TITLE), access.0.is_none(), "a prompt grant hides the title");
            assert_eq!(mirror.iter().any(|n| n.text == "secret"), access.0.is_none());
        }
    }
}
