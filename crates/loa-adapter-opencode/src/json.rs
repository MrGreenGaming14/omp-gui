//! Small helpers over `serde_json::Value` addressed by JSON pointers.

use serde_json::{Map, Value as Json};

pub fn str_at<'a>(v: &'a Json, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr)?.as_str()
}

/// An integer, accepting whole floats (JavaScript numbers).
pub fn int_at(v: &Json, ptr: &str) -> Option<i64> {
    let n = v.pointer(ptr)?;
    n.as_i64().or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
}

pub fn float_at(v: &Json, ptr: &str) -> Option<f64> {
    v.pointer(ptr)?.as_f64()
}

pub fn bool_at(v: &Json, ptr: &str) -> Option<bool> {
    v.pointer(ptr)?.as_bool()
}

/// A copy of `v` without the leaves at `ptrs`. Objects that become empty stay,
/// so putting the leaves back rebuilds the same structure.
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

/// Sets the leaf at `ptr`, creating objects on the way. A non-object in the
/// way is replaced by an object.
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

/// Compact JSON text, or an empty string for an empty object.
pub fn remainder_text(v: &Json) -> String {
    match v {
        Json::Object(map) if map.is_empty() => String::new(),
        _ => v.to_string(),
    }
}

/// Parses `text` as a JSON object; empty text is an empty object.
pub fn parse_object(text: &str) -> serde_json::Result<Json> {
    if text.is_empty() {
        return Ok(Json::Object(Map::new()));
    }
    serde_json::from_str(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn without_then_put_restores_nested_leaves() {
        let full = json!({"a": 1, "state": {"time": {"start": 5, "end": 6}, "m": {}}});
        let mut rest = without(&full, &["/a", "/state/time/start", "/state/time/end"]);
        assert_eq!(rest, json!({"state": {"time": {}, "m": {}}}));
        put(&mut rest, "/a", json!(1));
        put(&mut rest, "/state/time/start", json!(5));
        put(&mut rest, "/state/time/end", json!(6));
        assert_eq!(rest, full);
    }
}
