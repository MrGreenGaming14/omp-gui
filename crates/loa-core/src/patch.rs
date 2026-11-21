//! Patches: the only way a session DOM changes. A [`Draft`] is a wanted change;
//! the session's single writer orders it into a [`Patch`] (seq, ts, new id).

use crate::dom::NodeId;
use crate::schema::{ElementKind, TEXT};
use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type Attrs = BTreeMap<String, Value>;

/// One committed change. `seq` starts at 1 and has no gaps within a journal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Patch {
    pub seq: u64,
    /// The node the patch acts on. For `Insert` it is the id of the new node.
    pub node: NodeId,
    /// Unix milliseconds.
    pub ts: i64,
    pub op: Op,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
    /// Add a node under `parent`. `index` is the position among the parent's
    /// children, `None` appends. `parent` is [`NodeId::DOCUMENT`] for the root.
    Insert { parent: NodeId, index: Option<u32>, kind: ElementKind, attrs: Attrs, text: String },
    /// Set an attribute, or clear it with `None`. The name [`TEXT`] replaces
    /// the node text.
    Set { name: String, value: Option<Value> },
    /// Append to the node text. Used for streaming tokens.
    Append { text: String },
    /// Remove the node and its subtree.
    Remove,
}

/// A change that has not been ordered yet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Draft {
    Insert { parent: NodeId, index: Option<u32>, kind: ElementKind, attrs: Attrs, text: String },
    Set { node: NodeId, name: String, value: Option<Value> },
    Append { node: NodeId, text: String },
    Remove { node: NodeId },
}

impl Draft {
    /// Append a new `kind` node as the last child of `parent`.
    pub fn insert<N: Into<String>>(
        parent: NodeId,
        kind: ElementKind,
        attrs: impl IntoIterator<Item = (N, Value)>,
        text: impl Into<String>,
    ) -> Self {
        let attrs = attrs.into_iter().map(|(name, value)| (name.into(), value)).collect();
        Draft::Insert { parent, index: None, kind, attrs, text: text.into() }
    }

    /// Like [`Draft::insert`] at a position among the parent's children.
    pub fn insert_at<N: Into<String>>(
        parent: NodeId,
        index: u32,
        kind: ElementKind,
        attrs: impl IntoIterator<Item = (N, Value)>,
        text: impl Into<String>,
    ) -> Self {
        let attrs = attrs.into_iter().map(|(name, value)| (name.into(), value)).collect();
        Draft::Insert { parent, index: Some(index), kind, attrs, text: text.into() }
    }

    pub fn set(node: NodeId, name: impl Into<String>, value: impl Into<Value>) -> Self {
        Draft::Set { node, name: name.into(), value: Some(value.into()) }
    }

    pub fn clear(node: NodeId, name: impl Into<String>) -> Self {
        Draft::Set { node, name: name.into(), value: None }
    }

    /// Replace the node text.
    pub fn set_text(node: NodeId, text: impl Into<String>) -> Self {
        Self::set(node, TEXT, text.into())
    }

    pub fn append(node: NodeId, text: impl Into<String>) -> Self {
        Draft::Append { node, text: text.into() }
    }

    pub fn remove(node: NodeId) -> Self {
        Draft::Remove { node }
    }
}
