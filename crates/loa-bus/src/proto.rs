//! Protobuf wire format of patches and DOMs.
//!
//! The journal stores patches in this format and peers exchange them in it,
//! so this module knows nothing about files. Encode with prost through the
//! `From<&loa_core::…>` conversions; decode with [`decode_patch`] and
//! [`decode_dom`].
//!
//! Field numbers are frozen: change a message only by adding fields.

use loa_core::{Attrs, ElementKind, NodeId, PartsError};
use prost::Message as _;

/// An attribute value. Exactly one variant is set.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Value {
    #[prost(oneof = "value::Kind", tags = "1, 2, 3, 4, 5, 6")]
    pub kind: Option<value::Kind>,
}

pub mod value {
    /// The variants of [`super::Value`].
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(bool, tag = "1")]
        Bool(bool),
        #[prost(sint64, tag = "2")]
        Int(i64),
        #[prost(double, tag = "3")]
        Float(f64),
        #[prost(string, tag = "4")]
        Str(String),
        #[prost(bytes = "vec", tag = "5")]
        Bytes(Vec<u8>),
        #[prost(message, tag = "6")]
        List(super::ValueList),
    }
}

/// Items of a list value.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ValueList {
    #[prost(message, repeated, tag = "1")]
    pub items: Vec<Value>,
}

/// One named attribute. Attribute maps travel as repeated `Attr` sorted by name.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Attr {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(message, optional, tag = "2")]
    pub value: Option<Value>,
}

/// A committed patch.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Patch {
    #[prost(uint64, tag = "1")]
    pub seq: u64,
    #[prost(uint64, tag = "2")]
    pub node: u64,
    #[prost(int64, tag = "3")]
    pub ts: i64,
    #[prost(message, optional, tag = "4")]
    pub op: Option<Op>,
}

/// What a patch does. Exactly one variant is set.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Op {
    #[prost(oneof = "op::Kind", tags = "1, 2, 3, 4")]
    pub kind: Option<op::Kind>,
}

pub mod op {
    /// The variants of [`super::Op`].
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(message, tag = "1")]
        Insert(super::Insert),
        #[prost(message, tag = "2")]
        Set(super::Set),
        #[prost(message, tag = "3")]
        Append(super::Append),
        #[prost(message, tag = "4")]
        Remove(super::Remove),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Insert {
    #[prost(uint64, tag = "1")]
    pub parent: u64,
    #[prost(uint32, optional, tag = "2")]
    pub index: Option<u32>,
    /// Tag name of the element, see `ElementKind::as_str`.
    #[prost(string, tag = "3")]
    pub kind: String,
    #[prost(message, repeated, tag = "4")]
    pub attrs: Vec<Attr>,
    #[prost(string, tag = "5")]
    pub text: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Set {
    #[prost(string, tag = "1")]
    pub name: String,
    /// Absent clears the attribute.
    #[prost(message, optional, tag = "2")]
    pub value: Option<Value>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Append {
    #[prost(string, tag = "1")]
    pub text: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Remove {}

/// One DOM element.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Node {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(string, tag = "2")]
    pub kind: String,
    #[prost(uint64, tag = "3")]
    pub parent: u64,
    #[prost(uint64, repeated, tag = "4")]
    pub children: Vec<u64>,
    #[prost(message, repeated, tag = "5")]
    pub attrs: Vec<Attr>,
    #[prost(string, tag = "6")]
    pub text: String,
}

/// A whole DOM: every node, the next free id and the last applied sequence.
#[derive(Clone, PartialEq, prost::Message)]
pub struct Dom {
    #[prost(uint64, tag = "1")]
    pub seq: u64,
    #[prost(uint64, tag = "2")]
    pub next_id: u64,
    #[prost(message, repeated, tag = "3")]
    pub nodes: Vec<Node>,
}

/// Why wire data doesn't convert to `loa-core` types.
#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error(transparent)]
    Decode(#[from] prost::DecodeError),
    #[error("patch {seq} has no op")]
    MissingOp { seq: u64 },
    #[error("a value has no variant")]
    MissingValue,
    #[error("unknown element kind {0:?}")]
    UnknownKind(String),
    #[error("attribute {0:?} appears twice")]
    DuplicateAttr(String),
    #[error(transparent)]
    Parts(#[from] PartsError),
}

pub type Result<T, E = ProtoError> = std::result::Result<T, E>;

// Conversions from `loa-core`.

impl From<&loa_core::Value> for Value {
    fn from(v: &loa_core::Value) -> Self {
        use loa_core::Value as V;
        let kind = match v {
            V::Bool(b) => value::Kind::Bool(*b),
            V::Int(i) => value::Kind::Int(*i),
            V::Float(f) => value::Kind::Float(*f),
            V::Str(s) => value::Kind::Str(s.clone()),
            V::Bytes(b) => value::Kind::Bytes(b.clone()),
            V::List(items) => value::Kind::List(ValueList { items: items.iter().map(Value::from).collect() }),
        };
        Value { kind: Some(kind) }
    }
}

fn attrs_to_proto(attrs: &Attrs) -> Vec<Attr> {
    attrs.iter().map(|(name, value)| Attr { name: name.clone(), value: Some(value.into()) }).collect()
}

impl From<&loa_core::Op> for Op {
    fn from(op: &loa_core::Op) -> Self {
        use loa_core::Op as O;
        let kind = match op {
            O::Insert { parent, index, kind, attrs, text } => op::Kind::Insert(Insert {
                parent: parent.0,
                index: *index,
                kind: kind.as_str().to_owned(),
                attrs: attrs_to_proto(attrs),
                text: text.clone(),
            }),
            O::Set { name, value } => {
                op::Kind::Set(Set { name: name.clone(), value: value.as_ref().map(Value::from) })
            }
            O::Append { text } => op::Kind::Append(Append { text: text.clone() }),
            O::Remove => op::Kind::Remove(Remove {}),
        };
        Op { kind: Some(kind) }
    }
}

impl From<&loa_core::Patch> for Patch {
    fn from(p: &loa_core::Patch) -> Self {
        Patch { seq: p.seq, node: p.node.0, ts: p.ts, op: Some((&p.op).into()) }
    }
}

impl From<&loa_core::Node> for Node {
    fn from(n: &loa_core::Node) -> Self {
        Node {
            id: n.id.0,
            kind: n.kind.as_str().to_owned(),
            parent: n.parent.0,
            children: n.children.iter().map(|c| c.0).collect(),
            attrs: attrs_to_proto(&n.attrs),
            text: n.text.clone(),
        }
    }
}

impl From<&loa_core::Dom> for Dom {
    fn from(d: &loa_core::Dom) -> Self {
        Dom { seq: d.seq(), next_id: d.next_id().0, nodes: d.iter().map(Node::from).collect() }
    }
}

// Conversions to `loa-core`. They move strings and bytes out of the message.

impl TryFrom<Value> for loa_core::Value {
    type Error = ProtoError;

    fn try_from(v: Value) -> Result<Self> {
        use loa_core::Value as V;
        Ok(match v.kind.ok_or(ProtoError::MissingValue)? {
            value::Kind::Bool(b) => V::Bool(b),
            value::Kind::Int(i) => V::Int(i),
            value::Kind::Float(f) => V::Float(f),
            value::Kind::Str(s) => V::Str(s),
            value::Kind::Bytes(b) => V::Bytes(b),
            value::Kind::List(list) => {
                V::List(list.items.into_iter().map(loa_core::Value::try_from).collect::<Result<_>>()?)
            }
        })
    }
}

fn kind_from_proto(kind: String) -> Result<ElementKind> {
    ElementKind::parse(&kind).ok_or(ProtoError::UnknownKind(kind))
}

fn attrs_from_proto(attrs: Vec<Attr>) -> Result<Attrs> {
    use std::collections::btree_map::Entry;
    let mut out = Attrs::new();
    for attr in attrs {
        let value = attr.value.ok_or(ProtoError::MissingValue)?.try_into()?;
        match out.entry(attr.name) {
            Entry::Occupied(e) => return Err(ProtoError::DuplicateAttr(e.key().clone())),
            Entry::Vacant(e) => {
                e.insert(value);
            }
        }
    }
    Ok(out)
}

impl TryFrom<Op> for loa_core::Op {
    type Error = ProtoError;

    /// Fails with `MissingOp { seq: 0 }`; [`Patch`] conversion fills in the sequence.
    fn try_from(op: Op) -> Result<Self> {
        use loa_core::Op as O;
        Ok(match op.kind.ok_or(ProtoError::MissingOp { seq: 0 })? {
            op::Kind::Insert(i) => O::Insert {
                parent: NodeId(i.parent),
                index: i.index,
                kind: kind_from_proto(i.kind)?,
                attrs: attrs_from_proto(i.attrs)?,
                text: i.text,
            },
            op::Kind::Set(s) => O::Set { name: s.name, value: s.value.map(TryInto::try_into).transpose()? },
            op::Kind::Append(a) => O::Append { text: a.text },
            op::Kind::Remove(Remove {}) => O::Remove,
        })
    }
}

impl TryFrom<Patch> for loa_core::Patch {
    type Error = ProtoError;

    fn try_from(p: Patch) -> Result<Self> {
        let seq = p.seq;
        let op = match p.op.map(loa_core::Op::try_from) {
            None | Some(Err(ProtoError::MissingOp { .. })) => return Err(ProtoError::MissingOp { seq }),
            Some(op) => op?,
        };
        Ok(loa_core::Patch { seq, node: NodeId(p.node), ts: p.ts, op })
    }
}

impl TryFrom<Node> for loa_core::Node {
    type Error = ProtoError;

    fn try_from(n: Node) -> Result<Self> {
        Ok(loa_core::Node {
            id: NodeId(n.id),
            kind: kind_from_proto(n.kind)?,
            parent: NodeId(n.parent),
            children: n.children.into_iter().map(NodeId).collect(),
            attrs: attrs_from_proto(n.attrs)?,
            text: n.text,
        })
    }
}

impl TryFrom<Dom> for loa_core::Dom {
    type Error = ProtoError;

    fn try_from(d: Dom) -> Result<Self> {
        let nodes = d.nodes.into_iter().map(loa_core::Node::try_from).collect::<Result<Vec<_>>>()?;
        Ok(loa_core::Dom::from_parts(nodes, NodeId(d.next_id), d.seq)?)
    }
}

/// Decodes one encoded [`Patch`] message.
pub fn decode_patch(bytes: &[u8]) -> Result<loa_core::Patch> {
    Patch::decode(bytes)?.try_into()
}

/// Decodes one encoded [`Dom`] message and checks that it forms a tree.
pub fn decode_dom(bytes: &[u8]) -> Result<loa_core::Dom> {
    Dom::decode(bytes)?.try_into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_kind_and_missing_op_are_rejected() {
        let insert = Insert { kind: "blink".into(), ..Default::default() };
        let mut p = Patch { seq: 1, node: 1, ts: 0, op: Some(Op { kind: Some(op::Kind::Insert(insert)) }) };
        assert!(matches!(decode_patch(&p.encode_to_vec()), Err(ProtoError::UnknownKind(k)) if k == "blink"));
        p.op = None;
        assert!(matches!(decode_patch(&p.encode_to_vec()), Err(ProtoError::MissingOp { seq: 1 })));
    }
}
