//! In place editor for members of a JSON object document.
//!
//! Claude Code's `settings.json` is plain JSON that users edit by hand. Editing
//! it through a parse and reserialize would reorder and reformat every key, so
//! this module tokenizes just enough of the text (strings with escapes, nested
//! brackets) to find object members' key and value spans, and splices the one
//! edited member. A member is addressed by a key path from the root object
//! (`["model"]`, `["modelSettings", "opus", "effortLevel"]`). Every edit is
//! checked by parsing the result and comparing it with the intended document; a
//! mismatch is an [`Error::EditCheck`] and no text is returned.

use std::ops::Range;

use serde::Serialize;
use serde_json::ser::PrettyFormatter;
use serde_json::{Map, Value};

use crate::{Error, Result};

/// Text used for a file that is missing or holds only whitespace.
const EMPTY_OBJECT: &str = "{\n}\n";
/// Indentation unit used when the file gives no example to copy.
const DEFAULT_INDENT: &str = "  ";

/// Parses `text` as a JSON object. Empty or whitespace-only text is an empty
/// object. `what` names the file in errors.
pub(crate) fn parse_object(text: &str, what: &str) -> Result<Map<String, Value>> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    let value: Value = serde_json::from_str(text).map_err(|e| Error::Parse {
        what: what.to_owned(),
        message: e.to_string(),
    })?;
    match value {
        Value::Object(map) => Ok(map),
        other => Err(Error::Unsupported {
            what: what.to_owned(),
            reason: format!("the root is {} instead of an object", kind(&other)),
        }),
    }
}

/// Returns `text` with the member at `path` set to `value`. An existing value is
/// replaced in place; a missing member is appended after the last member of its
/// object with the same separator and indentation, creating missing parent
/// objects. A parent that exists but is not an object is
/// [`Error::Unsupported`].
pub(crate) fn set_path(text: &str, path: &[&str], value: &Value, what: &str) -> Result<String> {
    let base = if text.trim().is_empty() {
        EMPTY_OBJECT
    } else {
        text
    };
    let mut expected = parse_object(base, what)?;
    insert_at(&mut expected, path, value.clone(), what)?;
    let mut layout = scan_object(base, skip_ws(base.as_bytes(), 0), what)?;
    let unit = indent_unit(base, &layout);
    let mut is_root = true;
    let mut rest = path;
    let edited = loop {
        let (key, tail) = split_path(rest)?;
        let found = layout.members.iter().rev().find(|m| m.key == *key);
        match found {
            Some(member) if !tail.is_empty() => {
                let start = member.value_start;
                layout = scan_object(base, start, what)?;
                is_root = false;
                rest = tail;
            }
            Some(member) => {
                let pretty = layout.is_multiline(base);
                let indent = line_indent(base, member.key_start);
                let rendered = render(value, pretty, unit, indent, what)?;
                break splice(base, member.value_start..member.value_end, &rendered);
            }
            None => {
                let nested = nest(tail, value);
                break insert_member(base, &layout, key, &nested, unit, is_root, what)?;
            }
        }
    };
    check(&edited, &expected, what)?;
    Ok(edited)
}

/// Returns `text` without the member at `path` and its separating comma. Text
/// without that member is returned unchanged.
pub(crate) fn remove_path(text: &str, path: &[&str], what: &str) -> Result<String> {
    let mut expected = parse_object(text, what)?;
    if !remove_at(&mut expected, path) {
        return Ok(text.to_owned());
    }
    let mut layout = scan_object(text, skip_ws(text.as_bytes(), 0), what)?;
    let mut rest = path;
    let edited = loop {
        let (key, tail) = split_path(rest)?;
        let index = layout
            .members
            .iter()
            .rposition(|m| m.key == *key)
            .ok_or_else(|| Error::EditCheck {
                path: what.to_owned(),
            })?;
        if tail.is_empty() {
            break remove_member(text, &layout, index);
        }
        let start = layout.members[index].value_start;
        layout = scan_object(text, start, what)?;
        rest = tail;
    };
    check(&edited, &expected, what)?;
    Ok(edited)
}

fn split_path<'a, 'b>(path: &'a [&'b str]) -> Result<(&'a &'b str, &'a [&'b str])> {
    path.split_first()
        .ok_or_else(|| Error::invalid("path", "a JSON member path is empty"))
}

/// Sets `value` at `path` in `map`, creating missing parent objects.
fn insert_at(map: &mut Map<String, Value>, path: &[&str], value: Value, what: &str) -> Result<()> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| Error::invalid("path", "a JSON member path is empty"))?;
    let mut current = map;
    for key in parents {
        let entry = current
            .entry((*key).to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        current = match entry {
            Value::Object(inner) => inner,
            other => {
                return Err(Error::Unsupported {
                    what: what.to_owned(),
                    reason: format!("{key} is {} instead of an object", kind(other)),
                });
            }
        };
    }
    current.insert((*last).to_owned(), value);
    Ok(())
}

/// Removes the member at `path` from `map`; false when there is none.
fn remove_at(map: &mut Map<String, Value>, path: &[&str]) -> bool {
    let Some((last, parents)) = path.split_last() else {
        return false;
    };
    let mut current = map;
    for key in parents {
        current = match current.get_mut(*key) {
            Some(Value::Object(inner)) => inner,
            _ => return false,
        };
    }
    current.remove(*last).is_some()
}

/// `value` wrapped in one single-member object per key of `keys`, outermost
/// first.
fn nest(keys: &[&str], value: &Value) -> Value {
    keys.iter().rev().fold(value.clone(), |inner, key| {
        let mut map = Map::new();
        map.insert((*key).to_owned(), inner);
        Value::Object(map)
    })
}

/// Byte spans of one object member.
#[derive(Debug)]
struct Member {
    key: String,
    /// Opening quote of the key.
    key_start: usize,
    /// Just after the closing quote of the key.
    key_end: usize,
    value_start: usize,
    value_end: usize,
}

/// Byte positions of an object's braces and its members in order.
#[derive(Debug)]
struct Layout {
    open: usize,
    close: usize,
    members: Vec<Member>,
}

impl Layout {
    /// Whether the object spans several lines.
    fn is_multiline(&self, text: &str) -> bool {
        text[self.open + 1..self.close].contains('\n')
    }
}

/// Scans the object whose opening brace is at `start`.
fn scan_object(text: &str, start: usize, what: &str) -> Result<Layout> {
    let malformed = || Error::Parse {
        what: what.to_owned(),
        message: "unexpected text in an object".to_owned(),
    };
    let b = text.as_bytes();
    if b.get(start) != Some(&b'{') {
        return Err(Error::Unsupported {
            what: what.to_owned(),
            reason: "the edited value is not an object".to_owned(),
        });
    }
    let open = start;
    let mut i = start + 1;
    let mut members = Vec::new();
    loop {
        i = skip_ws(b, i);
        match b.get(i) {
            Some(b'}') if members.is_empty() => {
                return Ok(Layout {
                    open,
                    close: i,
                    members,
                });
            }
            Some(b'"') => {}
            _ => return Err(malformed()),
        }
        let key_start = i;
        let key_end = scan_string(b, key_start).ok_or_else(malformed)?;
        let key: String =
            serde_json::from_str(&text[key_start..key_end]).map_err(|_| malformed())?;
        i = skip_ws(b, key_end);
        if b.get(i) != Some(&b':') {
            return Err(malformed());
        }
        let value_start = skip_ws(b, i + 1);
        let value_end = scan_value(b, value_start).ok_or_else(malformed)?;
        members.push(Member {
            key,
            key_start,
            key_end,
            value_start,
            value_end,
        });
        i = skip_ws(b, value_end);
        match b.get(i) {
            Some(b',') => i += 1,
            Some(b'}') => {
                return Ok(Layout {
                    open,
                    close: i,
                    members,
                });
            }
            _ => return Err(malformed()),
        }
    }
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while matches!(b.get(i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

/// Given the opening quote at `start`, returns the position after the closing
/// quote.
fn scan_string(b: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    loop {
        match b.get(i)? {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
}

/// Returns the position just after the value starting at `start`.
fn scan_value(b: &[u8], start: usize) -> Option<usize> {
    match b.get(start)? {
        b'"' => scan_string(b, start),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut i = start;
            loop {
                match b.get(i)? {
                    b'"' => {
                        i = scan_string(b, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth = depth.checked_sub(1)?;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        _ => {
            let mut i = start;
            while let Some(c) = b.get(i) {
                if matches!(c, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                    break;
                }
                i += 1;
            }
            (i > start).then_some(i)
        }
    }
}

/// Appends member `key: value` to the object `layout`.
fn insert_member(
    text: &str,
    layout: &Layout,
    key: &str,
    value: &Value,
    unit: &str,
    is_root: bool,
    what: &str,
) -> Result<String> {
    let key = serde_json::to_string(key).map_err(|e| render_error(&e, what))?;
    let members = &layout.members;
    let Some(last) = members.last() else {
        let interior = layout.open + 1..layout.close;
        if is_root || text.contains('\n') {
            let outer = leading_ws(text, layout.open);
            let inner = format!("{outer}{unit}");
            let rendered = render(value, true, unit, &inner, what)?;
            let member = format!("\n{inner}{key}: {rendered}\n{outer}");
            return Ok(splice(text, interior, &member));
        }
        let rendered = render(value, false, unit, "", what)?;
        return Ok(splice(text, interior, &format!("{key}: {rendered}")));
    };
    if !layout.is_multiline(text) {
        let rendered = render(value, false, unit, "", what)?;
        return Ok(splice(text, last.value_end..last.value_end, &format!(",{key}:{rendered}")));
    }
    let gap_start = match members.len() {
        1 => layout.open + 1,
        n => after_comma(text, members[n - 2].value_end, last.key_start),
    };
    let separator = &text[gap_start..last.key_start];
    let colon = &text[last.key_end..last.value_start];
    let indent = separator.rfind('\n').map_or("", |i| &separator[i + 1..]);
    let rendered = render(value, separator.contains('\n'), unit, indent, what)?;
    let member = format!(",{separator}{key}{colon}{rendered}");
    Ok(splice(text, last.value_end..last.value_end, &member))
}

/// Deletes member `index` of `layout` with the comma that separates it.
fn remove_member(text: &str, layout: &Layout, index: usize) -> String {
    let members = &layout.members;
    if members.len() == 1 {
        let keep = if layout.is_multiline(text) {
            format!("\n{}", leading_ws(text, layout.open))
        } else {
            String::new()
        };
        splice(text, layout.open + 1..layout.close, &keep)
    } else if index + 1 < members.len() {
        splice(
            text,
            members[index].key_start..members[index + 1].key_start,
            "",
        )
    } else {
        splice(
            text,
            members[index - 1].value_end..members[index].value_end,
            "",
        )
    }
}

/// Position just after the comma between `from` and `to`.
fn after_comma(text: &str, from: usize, to: usize) -> usize {
    text[from..to].find(',').map_or(from, |i| from + i + 1)
}

/// The whitespace before `pos` on its line, or `""` when other text precedes it.
fn line_indent(text: &str, pos: usize) -> &str {
    let line_start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    let prefix = &text[line_start..pos];
    if prefix.bytes().all(|c| c == b' ' || c == b'\t') {
        prefix
    } else {
        ""
    }
}

/// The leading whitespace of the line containing `pos`.
fn leading_ws(text: &str, pos: usize) -> &str {
    let line_start = text[..pos].rfind('\n').map_or(0, |i| i + 1);
    let line = &text[line_start..pos];
    let end = line
        .bytes()
        .position(|c| c != b' ' && c != b'\t')
        .unwrap_or(line.len());
    &line[..end]
}

/// The indentation unit of the file: the indentation of the root's first
/// member, or [`DEFAULT_INDENT`].
fn indent_unit<'a>(text: &'a str, root: &Layout) -> &'a str {
    match root.members.first() {
        Some(first) if !line_indent(text, first.key_start).is_empty() => {
            line_indent(text, first.key_start)
        }
        _ => DEFAULT_INDENT,
    }
}

/// Serializes `value` for a member whose line is indented by `indent`: compact
/// on one line, or pretty printed with `unit` per level and continuation lines
/// shifted by `indent`.
fn render(value: &Value, pretty: bool, unit: &str, indent: &str, what: &str) -> Result<String> {
    if !pretty {
        return serde_json::to_string(value).map_err(|e| render_error(&e, what));
    }
    let mut buf = Vec::new();
    let formatter = PrettyFormatter::with_indent(unit.as_bytes());
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
    value
        .serialize(&mut ser)
        .map_err(|e| render_error(&e, what))?;
    let text = String::from_utf8(buf).map_err(|e| Error::Parse {
        what: what.to_owned(),
        message: e.to_string(),
    })?;
    Ok(text.replace('\n', &format!("\n{indent}")))
}

fn render_error(err: &serde_json::Error, what: &str) -> Error {
    Error::Parse {
        what: what.to_owned(),
        message: err.to_string(),
    }
}

fn splice(text: &str, range: Range<usize>, with: &str) -> String {
    let mut out = String::with_capacity(text.len() + with.len());
    out.push_str(&text[..range.start]);
    out.push_str(with);
    out.push_str(&text[range.end..]);
    out
}

fn check(edited: &str, expected: &Map<String, Value>, what: &str) -> Result<()> {
    match serde_json::from_str::<Value>(edited) {
        Ok(Value::Object(map)) if &map == expected => Ok(()),
        _ => Err(Error::EditCheck {
            path: what.to_owned(),
        }),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const WHAT: &str = "settings.json";

    fn set(text: &str, key: &str, value: Value) -> String {
        set_path(text, &[key], &value, WHAT).unwrap()
    }

    fn remove(text: &str, key: &str) -> String {
        remove_path(text, &[key], WHAT).unwrap()
    }

    #[test]
    fn insert_into_empty_object() {
        assert_eq!(set("{}", "model", json!("opus")), "{\n  \"model\": \"opus\"\n}");
        assert_eq!(
            set("{\n}\n", "model", json!("opus")),
            "{\n  \"model\": \"opus\"\n}\n"
        );
        assert_eq!(
            set("  \n", "model", json!("opus")),
            "{\n  \"model\": \"opus\"\n}\n"
        );
    }

    #[test]
    fn insert_after_members_copies_separator_and_indent() {
        let text = "{\n    \"a\": 1,\n    \"b\": [1, 2]\n}\n";
        assert_eq!(
            set(text, "model", json!("opus")),
            "{\n    \"a\": 1,\n    \"b\": [1, 2],\n    \"model\": \"opus\"\n}\n"
        );
        assert_eq!(
            set("{\n\t\"a\": true\n}", "m", json!(null)),
            "{\n\t\"a\": true,\n\t\"m\": null\n}"
        );
        assert_eq!(
            set("{\"a\":1}", "model", json!("opus")),
            "{\"a\":1,\"model\":\"opus\"}"
        );
    }

    #[test]
    fn insert_into_compact_nested_object_keeps_existing_spacing() {
        let text = "{\n  \"modelSettings\": {\n    \"opus\": {\"maxEffortLevel\": \"max\", \"autoCompactWindow\": 400000}\n  }\n}\n";
        assert_eq!(
            set_path(text, &["modelSettings", "opus", "effortLevel"], &json!("high"), WHAT).unwrap(),
            "{\n  \"modelSettings\": {\n    \"opus\": {\"maxEffortLevel\": \"max\", \"autoCompactWindow\": 400000,\"effortLevel\":\"high\"}\n  }\n}\n"
        );
        assert_eq!(
            set_path("{\"opus\": { \"max\": 1 }}", &["opus", "effortLevel"], &json!("low"), WHAT).unwrap(),
            "{\"opus\": { \"max\": 1,\"effortLevel\":\"low\" }}"
        );
    }

    #[test]
    fn inserted_object_value_uses_the_file_indentation() {
        let text = "{\n    \"a\": 1\n}\n";
        let value = json!({ "opus": { "effortLevel": "high" } });
        assert_eq!(
            set(text, "modelSettings", value),
            "{\n    \"a\": 1,\n    \"modelSettings\": {\n        \"opus\": {\n            \"effortLevel\": \"high\"\n        }\n    }\n}\n"
        );
    }

    #[test]
    fn replace_value_keeps_everything_else() {
        let text = "{ \"x\" : [ {\"model\": 1} ],\n  \"model\" :   \"sonnet\"  ,\n  \"y\": {}\n}";
        assert_eq!(
            set(text, "model", json!("opus")),
            "{ \"x\" : [ {\"model\": 1} ],\n  \"model\" :   \"opus\"  ,\n  \"y\": {}\n}"
        );
    }

    #[test]
    fn nested_path_edits_only_the_inner_member() {
        let text = "{\n  \"modelSettings\": {\n    \"zeta\": {\"effortLevel\": \"low\"},\n    \"opus\": {\n      \"maxEffortLevel\": \"max\"\n    }\n  }\n}\n";
        let path = ["modelSettings", "opus", "effortLevel"];
        assert_eq!(
            set_path(text, &path, &json!("high"), WHAT).unwrap(),
            "{\n  \"modelSettings\": {\n    \"zeta\": {\"effortLevel\": \"low\"},\n    \"opus\": {\n      \"maxEffortLevel\": \"max\",\n      \"effortLevel\": \"high\"\n    }\n  }\n}\n"
        );
        let path = ["modelSettings", "haiku", "effortLevel"];
        assert_eq!(
            set_path(text, &path, &json!("low"), WHAT).unwrap(),
            "{\n  \"modelSettings\": {\n    \"zeta\": {\"effortLevel\": \"low\"},\n    \"opus\": {\n      \"maxEffortLevel\": \"max\"\n    },\n    \"haiku\": {\n      \"effortLevel\": \"low\"\n    }\n  }\n}\n"
        );
        assert_eq!(
            remove_path(text, &["modelSettings", "zeta"], WHAT).unwrap(),
            "{\n  \"modelSettings\": {\n    \"opus\": {\n      \"maxEffortLevel\": \"max\"\n    }\n  }\n}\n"
        );
        assert_eq!(
            remove_path(text, &["modelSettings", "opus", "maxEffortLevel"], WHAT).unwrap(),
            "{\n  \"modelSettings\": {\n    \"zeta\": {\"effortLevel\": \"low\"},\n    \"opus\": {\n    }\n  }\n}\n"
        );
    }

    #[test]
    fn nested_path_into_empty_or_scalar_parent() {
        let text = "{\n  \"modelSettings\": {}\n}\n";
        let path = ["modelSettings", "opus", "effortLevel"];
        assert_eq!(
            set_path(text, &path, &json!("high"), WHAT).unwrap(),
            "{\n  \"modelSettings\": {\n    \"opus\": {\n      \"effortLevel\": \"high\"\n    }\n  }\n}\n"
        );
        let scalar = "{\"modelSettings\": 3}";
        let err = set_path(scalar, &path, &json!("high"), WHAT).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
    }

    #[test]
    fn remove_first_middle_last_and_only_member() {
        let text = "{\n  \"a\": 1,\n  \"b\": {\"c\": [2]},\n  \"d\": \"e\"\n}\n";
        assert_eq!(
            remove(text, "a"),
            "{\n  \"b\": {\"c\": [2]},\n  \"d\": \"e\"\n}\n"
        );
        assert_eq!(remove(text, "b"), "{\n  \"a\": 1,\n  \"d\": \"e\"\n}\n");
        assert_eq!(
            remove(text, "d"),
            "{\n  \"a\": 1,\n  \"b\": {\"c\": [2]}\n}\n"
        );
        assert_eq!(remove("{\n  \"a\": 1\n}\n", "a"), "{\n}\n");
        assert_eq!(remove("{\"a\":1}", "a"), "{}");
        assert_eq!(remove(text, "zz"), text);
        assert_eq!(remove("", "zz"), "");
    }

    #[test]
    fn strings_with_braces_quotes_and_escapes_do_not_confuse_the_scanner() {
        let text = "{\n  \"hooks\": {\"cmd\": \"echo \\\"}{\\\" ] \\\\\"},\n  \"k\\\"ey,}\": \"va,lue}\",\n  \"model\": \"a\"\n}\n";
        assert_eq!(
            remove(text, "model"),
            "{\n  \"hooks\": {\"cmd\": \"echo \\\"}{\\\" ] \\\\\"},\n  \"k\\\"ey,}\": \"va,lue}\"\n}\n"
        );
        assert_eq!(
            set(text, "k\"ey,}", json!("x")),
            "{\n  \"hooks\": {\"cmd\": \"echo \\\"}{\\\" ] \\\\\"},\n  \"k\\\"ey,}\": \"x\",\n  \"model\": \"a\"\n}\n"
        );
        let added = set(text, "new", json!("q\"}{"));
        assert!(added.ends_with(",\n  \"new\": \"q\\\"}{\"\n}\n"), "{added}");
    }

    #[test]
    fn duplicate_key_edits_the_member_that_takes_effect() {
        let text = "{\"model\": \"a\", \"model\": \"b\"}";
        assert_eq!(
            set(text, "model", json!("c")),
            "{\"model\": \"a\", \"model\": \"c\"}"
        );
        // Removing the last duplicate would expose the first one, so the check
        // refuses the edit.
        let err = remove_path(text, &["model"], WHAT).unwrap_err();
        assert!(matches!(err, Error::EditCheck { .. }), "{err:?}");
    }

    #[test]
    fn non_object_root_and_invalid_json_are_refused() {
        let err = set_path("[1, 2]", &["model"], &json!("opus"), WHAT).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
        let err = set_path("{\"a\": }", &["model"], &json!("opus"), WHAT).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
        let err = remove_path("// comment\n{}", &["model"], WHAT).unwrap_err();
        assert!(matches!(err, Error::Parse { .. }), "{err:?}");
    }
}
