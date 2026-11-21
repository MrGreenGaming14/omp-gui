//! Attribute values carried by DOM nodes and `set` patches.

use crate::dom::NodeId;
use crate::patch::Attrs;
use serde::{Deserialize, Serialize};

/// The wire-level type of a [`Value`]. The schema declares one per attribute.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ValueType {
    Bool,
    Int,
    Float,
    Str,
    Bytes,
    List,
}

/// One attribute value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Value>),
}

/// Per variant: the `as_*` accessor and the `From` impl of its payload.
macro_rules! variants {
    ($($variant:ident($ty:ty) => $as:ident -> $out:ty, |$v:ident| $get:expr;)*) => {
        impl Value {
            pub fn value_type(&self) -> ValueType {
                match self { $(Value::$variant(_) => ValueType::$variant),* }
            }
            $(pub fn $as(&self) -> Option<$out> {
                match self { Value::$variant($v) => Some($get), _ => None }
            })*
        }
        $(impl From<$ty> for Value {
            fn from(v: $ty) -> Self { Value::$variant(v) }
        })*
    };
}

variants! {
    Bool(bool) => as_bool -> bool, |v| *v;
    Int(i64) => as_int -> i64, |v| *v;
    Float(f64) => as_float -> f64, |v| *v;
    Str(String) => as_str -> &str, |v| v;
    Bytes(Vec<u8>) => as_bytes -> &[u8], |v| v;
    List(Vec<Value>) => as_list -> &[Value], |v| v;
}

impl Value {
    /// The strings of a list value, skipping non-string items.
    pub fn str_items(&self) -> impl Iterator<Item = &str> {
        self.as_list().unwrap_or_default().iter().filter_map(Value::as_str)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Str(v.to_owned())
    }
}

impl From<Vec<String>> for Value {
    fn from(v: Vec<String>) -> Self {
        Value::List(v.into_iter().map(Value::Str).collect())
    }
}

/// A node id as an `Int` attribute (ids stay far below `i64::MAX`).
impl From<NodeId> for Value {
    fn from(id: NodeId) -> Self {
        Value::Int(i64::try_from(id.0).unwrap_or(i64::MAX))
    }
}

/// Anything that lists `(name, value)` pairs: arrays, vecs, [`Attrs`].
pub trait IntoAttrs {
    fn into_attrs(self) -> Attrs;
}

impl<K: Into<String>, V: Into<Value>, I: IntoIterator<Item = (K, V)>> IntoAttrs for I {
    fn into_attrs(self) -> Attrs {
        self.into_iter().map(|(k, v)| (k.into(), v.into())).collect()
    }
}

/// Builds [`Attrs`]: `attrs! { ROLE: Role::User, "x-key": 1i64 }`. Bare names
/// are [`attr`](crate::attr) constants.
#[macro_export]
macro_rules! attrs {
    (@name $k:ident) => { $crate::attr::$k };
    (@name $k:literal) => { $k };
    ($($k:tt : $v:expr),* $(,)?) => {{
        #[allow(unused_mut)]
        let mut attrs = $crate::Attrs::new();
        $(attrs.insert($crate::attrs!(@name $k).to_owned(), $crate::Value::from($v));)*
        attrs
    }};
}
