//! Text encoding of `tool_result` and compaction content, which Claude Code
//! writes as a string, an array of blocks or not at all.
//!
//! Kinds stored in `x-claude-content`:
//! - `string`: the text is the string
//! - `texts`: an array of text blocks, joined with "\n"; `x-claude-split`
//!   holds the byte length of each text and the blocks without `text` are
//!   kept beside the element
//! - `json`: any other value, as JSON text
//! - `absent`: no content field, empty text

use serde_json::Value as J;

pub(super) const STRING: &str = "string";
pub(super) const TEXTS: &str = "texts";
pub(super) const JSON: &str = "json";
pub(super) const ABSENT: &str = "absent";

/// Content turned into element text.
pub(super) struct Encoded {
    pub text: String,
    pub kind: &'static str,
    /// `texts`: byte length of each text.
    pub split: Vec<i64>,
    /// `texts`: each block without `text`.
    pub items: Vec<J>,
}

pub(super) fn encode(content: Option<&J>) -> Encoded {
    let plain = |text: String, kind| Encoded { text, kind, split: Vec::new(), items: Vec::new() };
    let Some(content) = content else {
        return plain(String::new(), ABSENT);
    };
    match content {
        J::String(text) => plain(text.clone(), STRING),
        J::Array(blocks) if !blocks.is_empty() && blocks.iter().all(is_text_block) => {
            let mut text = String::new();
            let mut split = Vec::with_capacity(blocks.len());
            let mut items = Vec::with_capacity(blocks.len());
            for (i, block) in blocks.iter().enumerate() {
                let mut item = block.clone();
                if let Some(J::String(part)) = item.as_object_mut().and_then(|o| super::take_field(o, "text")) {
                    if i > 0 {
                        text.push('\n');
                    }
                    text.push_str(&part);
                    // A String is at most isize::MAX bytes.
                    split.push(part.len() as i64);
                }
                items.push(item);
            }
            Encoded { text, kind: TEXTS, split, items }
        }
        other => plain(other.to_string(), JSON),
    }
}

/// Rebuilds content from element text. `kind` is `None` for elements the app
/// created, which hold JSON when `json` is set and a string otherwise. Text
/// that no longer matches its `texts` split becomes one text block, and text
/// that no longer parses as JSON becomes a string.
pub(super) fn decode(kind: Option<&str>, text: &str, json: bool, split: &[i64], items: Vec<J>) -> Option<J> {
    let string = || J::String(text.to_owned());
    let parsed = || serde_json::from_str(text).unwrap_or_else(|_| string());
    match kind {
        Some(ABSENT) => None,
        Some(JSON) => Some(parsed()),
        Some(TEXTS) => Some(J::Array(split_texts(text, split, items).unwrap_or_else(|| {
            vec![serde_json::json!({ "type": "text", "text": text })]
        }))),
        Some(_) => Some(string()),
        None if json => Some(parsed()),
        None => Some(string()),
    }
}

fn is_text_block(block: &J) -> bool {
    block.get("type").and_then(J::as_str) == Some("text") && block.get("text").is_some_and(J::is_string)
}

fn split_texts(text: &str, split: &[i64], items: Vec<J>) -> Option<Vec<J>> {
    if split.len() != items.len() {
        return None;
    }
    let mut pos = 0usize;
    let mut out = Vec::with_capacity(items.len());
    for (i, (len, mut item)) in split.iter().zip(items).enumerate() {
        if i > 0 {
            if text.as_bytes().get(pos) != Some(&b'\n') {
                return None;
            }
            pos += 1;
        }
        let end = pos.checked_add(usize::try_from(*len).ok()?)?;
        let part = text.get(pos..end)?;
        item.as_object_mut()?.insert("text".to_owned(), J::String(part.to_owned()));
        out.push(item);
        pos = end;
    }
    (pos == text.len()).then_some(out)
}
