//! Lossless round trips of native records: the fields the DOM models are
//! taken out, the rest is kept as residual JSON on the node and merged back
//! on export. Multi-part text is joined for the DOM with its part lengths
//! recorded, so export can split it again.

use loa_core::{Node, Value};
use serde_json::{Map, Value as Json};

/// The attribute holding `harness`'s residual JSON.
pub fn attr_name(harness: &str) -> String {
    format!("x-{harness}")
}

/// Moves every field of `obj` not named in `keep` into the returned residual.
pub fn take(obj: &mut Map<String, Json>, keep: &[&str]) -> Map<String, Json> {
    let gone: Vec<String> = obj.keys().filter(|k| !keep.contains(&k.as_str())).cloned().collect();
    gone.into_iter().filter_map(|k| obj.remove_entry(&k)).collect()
}

/// The residual `harness` stored on `node`; empty when none or malformed.
pub fn restore(node: &Node, harness: &str) -> Map<String, Json> {
    match node.str_attr(&attr_name(harness)).map(serde_json::from_str) {
        Some(Ok(Json::Object(obj))) => obj,
        _ => Map::new(),
    }
}

/// Joins `parts` with `separator`; the lengths let [`split_parts`] undo it.
pub fn join_parts<'p>(parts: impl IntoIterator<Item = &'p str>, separator: &str) -> (String, Vec<Value>) {
    let parts: Vec<&str> = parts.into_iter().collect();
    let lengths = parts.iter().map(|p| Value::Int(p.len() as i64)).collect();
    (parts.join(separator), lengths)
}

/// Splits text [`join_parts`] produced; `None` when it was edited since.
pub fn split_parts<'t>(text: &'t str, separator: &str, lengths: &[Value]) -> Option<Vec<&'t str>> {
    let mut pieces = Vec::with_capacity(lengths.len());
    let mut rest = text;
    for (index, length) in lengths.iter().enumerate() {
        if index > 0 {
            rest = rest.strip_prefix(separator)?;
        }
        let length = usize::try_from(length.as_int()?).ok()?;
        if !rest.is_char_boundary(length) {
            return None;
        }
        let (piece, tail) = rest.split_at(length);
        pieces.push(piece);
        rest = tail;
    }
    rest.is_empty().then_some(pieces)
}
