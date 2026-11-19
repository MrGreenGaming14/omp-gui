//! The session DOM: a typed tree that only changes by applying patches.

use crate::patch::{Attrs, Draft, Op, Patch};
use crate::schema::{self, ElementKind, SchemaError, TEXT};
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Id of a node. Stable for the life of the session and never reused: every
/// insert takes an id above all ids the session has ever used.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct NodeId(pub u64);

impl NodeId {
    /// The parent of the root. No node has this id.
    pub const DOCUMENT: NodeId = NodeId(0);
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// One element. Read only outside this crate: a [`Dom`] hands out `&Node`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub kind: ElementKind,
    /// [`NodeId::DOCUMENT`] for the root.
    pub parent: NodeId,
    pub children: Vec<NodeId>,
    pub attrs: Attrs,
    pub text: String,
}

impl Node {
    pub fn attr(&self, name: &str) -> Option<&Value> {
        self.attrs.get(name)
    }

    pub fn str_attr(&self, name: &str) -> Option<&str> {
        self.attr(name)?.as_str()
    }

    pub fn int_attr(&self, name: &str) -> Option<i64> {
        self.attr(name)?.as_int()
    }

    pub fn bool_attr(&self, name: &str) -> Option<bool> {
        self.attr(name)?.as_bool()
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ApplyError {
    #[error("patch seq {got} applied to a DOM that expects {expected}")]
    Sequence { expected: u64, got: u64 },
    #[error("node {0} doesn't exist")]
    MissingNode(NodeId),
    #[error("node id {got} was used before, the next free id is {next}")]
    IdReused { got: NodeId, next: NodeId },
    #[error("the document already has a root")]
    RootExists,
    #[error("index {index} is past the {len} children of node {parent}")]
    Index { parent: NodeId, index: u32, len: usize },
    #[error(transparent)]
    Schema(#[from] SchemaError),
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum PartsError {
    #[error("node {0} appears twice")]
    Duplicate(NodeId),
    #[error("node {parent} lists child {child}, which is missing, listed twice or names another parent")]
    Link { child: NodeId, parent: NodeId },
    #[error("node {0} can't be reached from the root")]
    Unreachable(NodeId),
    #[error("expected one root, found {0}")]
    Roots(usize),
    #[error("next id {next} isn't above node {max}")]
    NextId { next: NodeId, max: NodeId },
}

/// A session tree. Build one by applying patches from sequence 1, or from a
/// snapshot with [`Dom::from_parts`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Dom {
    nodes: HashMap<NodeId, Node>,
    root: Option<NodeId>,
    /// Highest id ever inserted. Ids at or below it are never accepted again.
    last_id: u64,
    seq: u64,
}

impl Dom {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds a DOM from snapshot data: every node, the next free id and the
    /// sequence of the last applied patch.
    pub fn from_parts(nodes: impl IntoIterator<Item = Node>, next_id: NodeId, seq: u64) -> Result<Self, PartsError> {
        let mut map = HashMap::new();
        for node in nodes {
            let id = node.id;
            if map.insert(id, node).is_some() {
                return Err(PartsError::Duplicate(id));
            }
        }
        let mut roots = map.values().filter(|n| n.parent == NodeId::DOCUMENT);
        let root = roots.next().map(|n| n.id);
        let extra = roots.count();
        if extra > 0 || (root.is_none() && !map.is_empty()) {
            return Err(PartsError::Roots(extra + usize::from(root.is_some())));
        }
        // Walk down from the root: every listed child must exist, name the
        // lister as its parent and be listed once. Whatever the walk misses
        // is detached from the tree.
        let mut seen = std::collections::HashSet::with_capacity(map.len());
        let mut stack: Vec<NodeId> = root.into_iter().collect();
        seen.extend(root);
        while let Some(parent) = stack.pop() {
            for child in &map[&parent].children {
                let linked = map.get(child).is_some_and(|n| n.parent == parent) && seen.insert(*child);
                if !linked {
                    return Err(PartsError::Link { child: *child, parent });
                }
                stack.push(*child);
            }
        }
        if let Some(lost) = map.keys().find(|id| !seen.contains(id)) {
            return Err(PartsError::Unreachable(*lost));
        }
        let max = map.keys().copied().max().unwrap_or(NodeId::DOCUMENT);
        if next_id <= max {
            return Err(PartsError::NextId { next: next_id, max });
        }
        Ok(Self { nodes: map, root, last_id: next_id.0 - 1, seq })
    }

    /// Sequence number of the last applied patch. 0 for a new DOM.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// The lowest id an insert may use.
    pub fn next_id(&self) -> NodeId {
        NodeId(self.last_id + 1)
    }

    pub fn root(&self) -> Option<&Node> {
        self.get(self.root?)
    }

    pub fn get(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(&id)
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Children of `id` in order. Empty when `id` doesn't exist.
    pub fn children(&self, id: NodeId) -> impl Iterator<Item = &Node> {
        let ids = self.get(id).map_or(&[][..], |n| &n.children);
        ids.iter().filter_map(|c| self.get(*c))
    }

    /// Children of `id` of one kind, in order.
    pub fn children_of_kind(&self, id: NodeId, kind: ElementKind) -> impl Iterator<Item = &Node> {
        self.children(id).filter(move |n| n.kind == kind)
    }

    /// `id` and everything under it, parents before children, in document order.
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants { dom: self, stack: self.get(id).map(|n| n.id).into_iter().collect() }
    }

    /// Every node in document order.
    pub fn iter(&self) -> Descendants<'_> {
        Descendants { dom: self, stack: self.root.into_iter().collect() }
    }

    /// Parents of `id` from the nearest up to the root.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = &Node> {
        let mut next = self.get(id).map(|n| n.parent);
        std::iter::from_fn(move || {
            let node = self.get(next?)?;
            next = Some(node.parent);
            Some(node)
        })
    }

    /// Orders `draft` as the next patch without applying it.
    pub fn prepare(&self, draft: Draft, ts: i64) -> Patch {
        let seq = self.seq + 1;
        match draft {
            Draft::Insert { parent, index, kind, attrs, text } => {
                Patch { seq, node: self.next_id(), ts, op: Op::Insert { parent, index, kind, attrs, text } }
            }
            Draft::Set { node, name, value } => Patch { seq, node, ts, op: Op::Set { name, value } },
            Draft::Append { node, text } => Patch { seq, node, ts, op: Op::Append { text } },
            Draft::Remove { node } => Patch { seq, node, ts, op: Op::Remove },
        }
    }

    /// Orders `draft` as the next patch and applies it.
    pub fn commit(&mut self, draft: Draft, ts: i64) -> Result<Patch, ApplyError> {
        let patch = self.prepare(draft, ts);
        self.apply(&patch)?;
        Ok(patch)
    }

    /// Applies the next patch. On error the DOM is unchanged.
    pub fn apply(&mut self, patch: &Patch) -> Result<(), ApplyError> {
        if patch.seq != self.seq + 1 {
            return Err(ApplyError::Sequence { expected: self.seq + 1, got: patch.seq });
        }
        let id = patch.node;
        match &patch.op {
            Op::Insert { parent, index, kind, attrs, text } => {
                if id.0 <= self.last_id {
                    return Err(ApplyError::IdReused { got: id, next: self.next_id() });
                }
                let parent_kind = if *parent == NodeId::DOCUMENT {
                    if self.root.is_some() {
                        return Err(ApplyError::RootExists);
                    }
                    None
                } else {
                    let node = self.get(*parent).ok_or(ApplyError::MissingNode(*parent))?;
                    if let Some(index) = *index
                        && index as usize > node.children.len()
                    {
                        return Err(ApplyError::Index { parent: *parent, index, len: node.children.len() });
                    }
                    Some(node.kind)
                };
                schema::validate_insert(parent_kind, *kind, attrs.iter().map(|(k, v)| (k.as_str(), v)))?;
                match self.nodes.get_mut(parent) {
                    Some(node) => {
                        let at = index.map_or(node.children.len(), |i| i as usize);
                        node.children.insert(at, id);
                    }
                    None => self.root = Some(id),
                }
                let node = Node {
                    id,
                    kind: *kind,
                    parent: *parent,
                    children: Vec::new(),
                    attrs: attrs.clone(),
                    text: text.clone(),
                };
                self.nodes.insert(id, node);
                self.last_id = id.0;
            }
            Op::Set { name, value } => {
                let node = self.nodes.get_mut(&id).ok_or(ApplyError::MissingNode(id))?;
                schema::validate_set(node.kind, name, value.as_ref())?;
                match (name.as_str(), value) {
                    (TEXT, Some(Value::Str(text))) => node.text.clone_from(text),
                    (TEXT, _) => node.text.clear(),
                    (_, Some(value)) => {
                        node.attrs.insert(name.clone(), value.clone());
                    }
                    (_, None) => {
                        node.attrs.remove(name);
                    }
                }
            }
            Op::Append { text } => {
                let node = self.nodes.get_mut(&id).ok_or(ApplyError::MissingNode(id))?;
                node.text.push_str(text);
            }
            Op::Remove => {
                let parent = self.get(id).ok_or(ApplyError::MissingNode(id))?.parent;
                let doomed: Vec<NodeId> = self.descendants(id).map(|n| n.id).collect();
                for gone in doomed {
                    self.nodes.remove(&gone);
                }
                match self.nodes.get_mut(&parent) {
                    Some(node) => node.children.retain(|c| *c != id),
                    None => self.root = None,
                }
            }
        }
        self.seq = patch.seq;
        Ok(())
    }
}

/// Iterator of [`Dom::descendants`] and [`Dom::iter`].
pub struct Descendants<'a> {
    dom: &'a Dom,
    stack: Vec<NodeId>,
}

impl<'a> Iterator for Descendants<'a> {
    type Item = &'a Node;

    fn next(&mut self) -> Option<&'a Node> {
        let node = self.dom.get(self.stack.pop()?)?;
        self.stack.extend(node.children.iter().rev());
        Some(node)
    }
}
