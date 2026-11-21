//! Small helpers over `serde_json` values and RFC 3339 timestamps.

use serde_json::{Map, Value};

/// The string at `key`, when `value` is an object holding a string there.
pub(crate) fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

/// The integer at `key`.
pub(crate) fn int_at(value: &Value, key: &str) -> Option<i64> {
    value.get(key)?.as_i64()
}

/// Removes `key` from `object` when it holds a string and returns the string.
pub(crate) fn take_str(object: &mut Map<String, Value>, key: &str) -> Option<String> {
    match object.get(key) {
        Some(Value::String(_)) => match object.remove(key) {
            Some(Value::String(text)) => Some(text),
            _ => None,
        },
        _ => None,
    }
}
