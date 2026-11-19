//! Protobuf wire format of patches and DOMs.
//!
//! The journal stores patches in this format and peers exchange them in it,
//! so this module knows nothing about files. The prost messages are the
//! schema; [`encode_patch`] and [`encode_dom`] write the same bytes straight
//! from borrowed `loa-core` values, without building the messages first.
//!
//! Field numbers are frozen: change a message only by adding fields.

use loa_core::{Attrs, ElementKind, NodeId, PartsError};
use prost::Message as _;
use prost::bytes::BufMut;

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

/// Length of [`encode_patch`]'s output.
pub fn encoded_patch_len(patch: &loa_core::Patch) -> usize {
    wire::patch_len(patch)
}

/// Writes `patch` as a [`Patch`] message.
pub fn encode_patch(patch: &loa_core::Patch, buf: &mut impl BufMut) {
    wire::put_patch(patch, buf);
}

/// Writes `dom` as a [`Dom`] message, nodes in document order.
pub fn encode_dom(dom: &loa_core::Dom, buf: &mut impl BufMut) {
    wire::put_dom(dom, buf);
}

/// Protobuf encoding of borrowed `loa-core` values. Mirrors the derived
/// messages above field for field, including proto3's omission of zero
/// scalars and empty strings outside oneofs.
mod wire {
    use loa_core::{Attrs, Dom, Node, Op, Patch, Value};
    use prost::bytes::BufMut;

    const VARINT: u32 = 0;
    const I64: u32 = 1;
    const LEN: u32 = 2;

    fn varint_len(v: u64) -> usize {
        (64 - (v | 1).leading_zeros() as usize).div_ceil(7)
    }

    fn put_varint(mut v: u64, buf: &mut impl BufMut) {
        while v >= 0x80 {
            buf.put_u8((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
        buf.put_u8(v as u8);
    }

    fn key_len(tag: u32) -> usize {
        varint_len(u64::from(tag << 3))
    }

    fn put_key(tag: u32, wire_type: u32, buf: &mut impl BufMut) {
        put_varint(u64::from(tag << 3 | wire_type), buf);
    }

    /// A length delimited field holding `len` bytes.
    fn field_len(tag: u32, len: usize) -> usize {
        key_len(tag) + varint_len(len as u64) + len
    }

    fn put_len(tag: u32, len: usize, buf: &mut impl BufMut) {
        put_key(tag, LEN, buf);
        put_varint(len as u64, buf);
    }

    fn uint_len(tag: u32, v: u64) -> usize {
        if v == 0 { 0 } else { key_len(tag) + varint_len(v) }
    }

    fn put_uint(tag: u32, v: u64, buf: &mut impl BufMut) {
        if v != 0 {
            put_key(tag, VARINT, buf);
            put_varint(v, buf);
        }
    }

    fn str_len(tag: u32, s: &str) -> usize {
        if s.is_empty() { 0 } else { field_len(tag, s.len()) }
    }

    fn put_str(tag: u32, s: &str, buf: &mut impl BufMut) {
        if !s.is_empty() {
            put_len(tag, s.len(), buf);
            buf.put_slice(s.as_bytes());
        }
    }

    fn zigzag(v: i64) -> u64 {
        ((v << 1) ^ (v >> 63)) as u64
    }

    fn value_len(v: &Value) -> usize {
        match v {
            Value::Bool(_) => key_len(1) + 1,
            Value::Int(i) => key_len(2) + varint_len(zigzag(*i)),
            Value::Float(_) => key_len(3) + 8,
            Value::Str(s) => field_len(4, s.len()),
            Value::Bytes(b) => field_len(5, b.len()),
            Value::List(items) => field_len(6, list_len(items)),
        }
    }

    fn list_len(items: &[Value]) -> usize {
        items.iter().map(|item| field_len(1, value_len(item))).sum()
    }

    fn put_value(v: &Value, buf: &mut impl BufMut) {
        match v {
            Value::Bool(b) => {
                put_key(1, VARINT, buf);
                buf.put_u8(u8::from(*b));
            }
            Value::Int(i) => {
                put_key(2, VARINT, buf);
                put_varint(zigzag(*i), buf);
            }
            Value::Float(f) => {
                put_key(3, I64, buf);
                buf.put_f64_le(*f);
            }
            Value::Str(s) => {
                put_len(4, s.len(), buf);
                buf.put_slice(s.as_bytes());
            }
            Value::Bytes(b) => {
                put_len(5, b.len(), buf);
                buf.put_slice(b);
            }
            Value::List(items) => {
                put_len(6, list_len(items), buf);
                for item in items {
                    put_len(1, value_len(item), buf);
                    put_value(item, buf);
                }
            }
        }
    }

    fn attr_len(name: &str, value: &Value) -> usize {
        str_len(1, name) + field_len(2, value_len(value))
    }

    fn attrs_len(tag: u32, attrs: &Attrs) -> usize {
        attrs.iter().map(|(name, value)| field_len(tag, attr_len(name, value))).sum()
    }

    fn put_attrs(tag: u32, attrs: &Attrs, buf: &mut impl BufMut) {
        for (name, value) in attrs {
            put_len(tag, attr_len(name, value), buf);
            put_str(1, name, buf);
            put_len(2, value_len(value), buf);
            put_value(value, buf);
        }
    }

    /// Tag of the op variant and the length of its message.
    fn op_variant(op: &Op) -> (u32, usize) {
        match op {
            Op::Insert { parent, index, kind, attrs, text } => {
                let index_len = index.map_or(0, |i| key_len(2) + varint_len(u64::from(i)));
                let len = uint_len(1, parent.0)
                    + index_len
                    + str_len(3, kind.as_str())
                    + attrs_len(4, attrs)
                    + str_len(5, text);
                (1, len)
            }
            Op::Set { name, value } => (2, str_len(1, name) + value.as_ref().map_or(0, |v| field_len(2, value_len(v)))),
            Op::Append { text } => (3, str_len(1, text)),
            Op::Remove => (4, 0),
        }
    }

    pub(super) fn patch_len(p: &Patch) -> usize {
        let (tag, len) = op_variant(&p.op);
        uint_len(1, p.seq) + uint_len(2, p.node.0) + uint_len(3, p.ts as u64) + field_len(4, field_len(tag, len))
    }

    pub(super) fn put_patch(p: &Patch, buf: &mut impl BufMut) {
        put_uint(1, p.seq, buf);
        put_uint(2, p.node.0, buf);
        put_uint(3, p.ts as u64, buf);
        let (tag, len) = op_variant(&p.op);
        put_len(4, field_len(tag, len), buf);
        put_len(tag, len, buf);
        match &p.op {
            Op::Insert { parent, index, kind, attrs, text } => {
                put_uint(1, parent.0, buf);
                if let Some(i) = index {
                    put_key(2, VARINT, buf);
                    put_varint(u64::from(*i), buf);
                }
                put_str(3, kind.as_str(), buf);
                put_attrs(4, attrs, buf);
                put_str(5, text, buf);
            }
            Op::Set { name, value } => {
                put_str(1, name, buf);
                if let Some(v) = value {
                    put_len(2, value_len(v), buf);
                    put_value(v, buf);
                }
            }
            Op::Append { text } => put_str(1, text, buf),
            Op::Remove => {}
        }
    }

    fn children_len(n: &Node) -> usize {
        n.children.iter().map(|c| varint_len(c.0)).sum()
    }

    fn node_len(n: &Node) -> usize {
        let children = children_len(n);
        uint_len(1, n.id.0)
            + str_len(2, n.kind.as_str())
            + uint_len(3, n.parent.0)
            + if children == 0 { 0 } else { field_len(4, children) }
            + attrs_len(5, &n.attrs)
            + str_len(6, &n.text)
    }

    pub(super) fn put_dom(dom: &Dom, buf: &mut impl BufMut) {
        put_uint(1, dom.seq(), buf);
        put_uint(2, dom.next_id().0, buf);
        for n in dom.iter() {
            put_len(3, node_len(n), buf);
            put_uint(1, n.id.0, buf);
            put_str(2, n.kind.as_str(), buf);
            put_uint(3, n.parent.0, buf);
            let children = children_len(n);
            if children != 0 {
                put_len(4, children, buf);
                for c in &n.children {
                    put_varint(c.0, buf);
                }
            }
            put_attrs(5, &n.attrs, buf);
            put_str(6, &n.text, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, attr};

    fn every_value() -> Vec<(&'static str, loa_core::Value)> {
        use loa_core::Value as V;
        vec![
            (attr::HARNESS, V::from("omp")),
            ("x-bool", V::Bool(false)),
            ("x-int", V::Int(-1)),
            ("x-big", V::Int(i64::MIN)),
            ("x-zero", V::Int(0)),
            ("x-float", V::Float(-0.0)),
            ("x-empty", V::from("")),
            ("x-bytes", V::Bytes(vec![0, 255, 7])),
            ("x-list", V::List(vec![V::from("a"), V::List(vec![]), V::Int(300), V::Bytes(vec![])])),
        ]
    }

    fn sample_patches() -> Vec<loa_core::Patch> {
        let mut dom = loa_core::Dom::new();
        let mut out = Vec::new();
        let mut commit = |dom: &mut loa_core::Dom, d: Draft, ts: i64| {
            let p = dom.commit(d, ts).unwrap();
            out.push(p.clone());
            p.node
        };
        let session = commit(
            &mut dom,
            Draft::insert(
                NodeId::DOCUMENT,
                ElementKind::Session,
                [
                    (attr::ID, loa_core::Value::from("s")),
                    (attr::HARNESS, "omp".into()),
                    (attr::NODE, "n".into()),
                    (attr::CREATED, loa_core::Value::Int(0)),
                ],
                "",
            ),
            0,
        );
        let ext = commit(&mut dom, Draft::insert(session, ElementKind::Ext, every_value(), "raw ☃"), -5);
        commit(&mut dom, Draft::insert_at(session, 0, ElementKind::Ext, [(attr::HARNESS, loa_core::Value::from("x"))], ""), i64::MAX);
        commit(&mut dom, Draft::append(ext, " tail"), 1);
        commit(&mut dom, Draft::set(ext, "x-int", loa_core::Value::Int(i64::MAX)), 1);
        commit(&mut dom, Draft::clear(ext, "x-bool"), 1);
        commit(&mut dom, Draft::set_text(ext, ""), 1);
        commit(&mut dom, Draft::remove(ext), 1);
        out
    }

    #[test]
    fn patches_round_trip_through_both_encoders() {
        for patch in sample_patches() {
            let mut borrowed = Vec::new();
            encode_patch(&patch, &mut borrowed);
            let message = Patch::from(&patch).encode_to_vec();
            assert_eq!(borrowed, message, "borrowed encoder differs from prost for {patch:?}");
            assert_eq!(encoded_patch_len(&patch), borrowed.len());
            assert_eq!(decode_patch(&borrowed).unwrap(), patch);
        }
    }

    #[test]
    fn dom_round_trip_keeps_next_id_after_removals() {
        let mut dom = loa_core::Dom::new();
        for p in sample_patches() {
            dom.apply(&p).unwrap();
        }
        let mut borrowed = Vec::new();
        encode_dom(&dom, &mut borrowed);
        assert_eq!(borrowed, Dom::from(&dom).encode_to_vec());
        let back = decode_dom(&borrowed).unwrap();
        assert_eq!(back, dom);
        // The removed node's id stays burnt after a round trip.
        assert_eq!(back.next_id(), NodeId(4));
    }

    #[test]
    fn unknown_kind_and_missing_op_are_rejected() {
        let mut p = Patch::from(&sample_patches()[0]);
        if let Some(Op { kind: Some(op::Kind::Insert(i)) }) = &mut p.op {
            i.kind = "blink".into();
        }
        assert!(matches!(decode_patch(&p.encode_to_vec()), Err(ProtoError::UnknownKind(k)) if k == "blink"));
        p.op = None;
        assert!(matches!(decode_patch(&p.encode_to_vec()), Err(ProtoError::MissingOp { seq: 1 })));
    }
}
