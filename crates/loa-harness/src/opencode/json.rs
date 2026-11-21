//! JSON pointer helpers for the native objects.

use serde_json::{Map, Value as Json};

pub fn str_at<'a>(v: &'a Json, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr)?.as_str()
}

/// An integer, accepting whole floats (JavaScript numbers).
pub fn int_at(v: &Json, ptr: &str) -> Option<i64> {
    let n = v.pointer(ptr)?;
    n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
}

pub fn is_true(v: &Json, ptr: &str) -> bool {
    v.pointer(ptr).and_then(Json::as_bool) == Some(true)
}

/// A copy of `v` without the leaves at `ptrs`; emptied objects stay, so [`put`] rebuilds the same shape.
pub fn without(v: &Json, ptrs: &[&str]) -> Json {
    let mut copy = v.clone();
    for ptr in ptrs {
        if let Some((parent, key)) = ptr.rsplit_once('/')
            && let Some(Json::Object(map)) = copy.pointer_mut(parent)
        {
            map.remove(key);
        }
    }
    copy
}

/// Sets the leaf at `ptr`, creating (or replacing non-objects with) objects on the way.
pub fn put(v: &mut Json, ptr: &str, value: Json) {
    let mut cur = v;
    let mut keys = ptr.split('/').skip(1).peekable();
    while let Some(key) = keys.next() {
        if !cur.is_object() {
            *cur = Json::Object(Map::new());
        }
        let Json::Object(map) = cur else { return };
        if keys.peek().is_none() {
            map.insert(key.to_owned(), value);
            return;
        }
        cur = map.entry(key.to_owned()).or_insert_with(|| Json::Object(Map::new()));
    }
}

/// Compact JSON of `v` without `ptrs`; empty for an empty object.
pub fn rest(v: &Json, ptrs: &[&str]) -> String {
    match without(v, ptrs) {
        Json::Object(map) if map.is_empty() => String::new(),
        other => other.to_string(),
    }
}

/// Parses `text` as a JSON object; empty text is an empty object.
pub fn parse_object(text: &str) -> serde_json::Result<Json> {
    if text.is_empty() { Ok(Json::Object(Map::new())) } else { serde_json::from_str(text) }
}
