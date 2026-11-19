//! DOM to omp JSONL session file.

use super::{X_ENTRY, X_LINE, time, to_line};
use loa_adapter::AdapterError;
use loa_core::{Dom, ElementKind, MessageKind, Node, NodeId, Role, attr};
use serde_json::{Map, Value as Json, json};
use std::collections::HashSet;

pub(super) fn run(dom: &Dom) -> Result<String, AdapterError> {
    let session = dom
        .root()
        .filter(|node| node.kind == ElementKind::Session)
        .ok_or_else(|| AdapterError::Format("the DOM has no <session> root".to_owned()))?;

    let mut raws: Vec<(i64, &str)> = Vec::new();
    for node in dom.iter() {
        let Some(line) = node.int_attr(X_LINE) else { continue };
        let raw = if node.kind == ElementKind::Ext {
            node.text.as_str()
        } else {
            node.str_attr(X_ENTRY).ok_or_else(|| {
                AdapterError::Format(format!("<{}> {} has {X_LINE} but no {X_ENTRY}", node.kind, node.id))
            })?
        };
        raws.push((line, raw));
    }
    raws.sort_by_key(|(line, _)| *line);

    let mut out = String::new();
    let mut writer = Writer { parent: Json::Null, used: HashSet::new(), first_kept: Vec::new(), entries: Vec::new() };
    for (_, raw) in &raws {
        out.push_str(raw);
        out.push('\n');
        let parsed = serde_json::from_str::<Json>(raw).ok();
        if let Some(id) = parsed.as_ref().and_then(|entry| entry.get("id")).and_then(Json::as_str) {
            writer.used.insert(id.to_owned());
        }
    }
    match raws.last() {
        Some((_, raw)) => {
            // The last entry in the file is the leaf of the imported active path.
            let leaf = serde_json::from_str::<Json>(raw).ok();
            let is_entry = leaf
                .as_ref()
                .and_then(|leaf| leaf.get("type"))
                .and_then(Json::as_str)
                .is_some_and(|kind| kind != "title" && kind != "session");
            if is_entry && let Some(id) = leaf.as_ref().and_then(|leaf| leaf.get("id")).filter(|id| id.is_string()) {
                writer.parent = id.clone();
            }
        }
        None => {
            let id = session.str_attr(attr::NATIVE_ID).or_else(|| session.str_attr(attr::ID)).unwrap_or_default();
            let mut header = json!({
                "type": "session",
                "version": 3,
                "id": id,
                "timestamp": time::format_iso(session.int_attr(attr::CREATED).unwrap_or(0)),
                "cwd": session.str_attr(attr::CWD).unwrap_or_default(),
            });
            if let (Some(title), Json::Object(fields)) = (session.str_attr(attr::TITLE), &mut header) {
                fields.insert("title".to_owned(), Json::String(title.to_owned()));
            }
            out.push_str(&to_line(&header)?);
            out.push('\n');
        }
    }

    for child in dom.children(session.id) {
        match child.kind {
            ElementKind::Turn if !dom.descendants(child.id).any(|node| node.attrs.contains_key(X_LINE)) => {
                writer.turn(dom, child);
            }
            ElementKind::Compaction if !child.attrs.contains_key(X_LINE) => writer.compaction(child),
            _ => {}
        }
    }
    for entry in &writer.entries {
        out.push_str(&to_line(entry)?);
        out.push('\n');
    }
    Ok(out)
}

/// Builds entries for DOM content that has no raw line.
struct Writer {
    /// Id of the last entry written, the parent of the next one.
    parent: Json,
    /// Entry ids already in the file.
    used: HashSet<String>,
    /// Indices of synthesized compactions waiting for the next entry's id.
    first_kept: Vec<usize>,
    entries: Vec<Map<String, Json>>,
}

impl Writer {
    /// Appends an entry with the base fields and links it to the previous one.
    fn emit(&mut self, node: NodeId, ts: i64, kind: &str, fields: Vec<(&str, Json)>) {
        let base = format!("{:08x}", node.0);
        let mut id = base.clone();
        let mut suffix = 1;
        while !self.used.insert(id.clone()) {
            id = format!("{base}-{suffix}");
            suffix += 1;
        }
        for index in self.first_kept.drain(..) {
            if let Some(compaction) = self.entries.get_mut(index) {
                compaction.insert("firstKeptEntryId".to_owned(), Json::String(id.clone()));
            }
        }
        let mut entry = Map::new();
        entry.insert("type".to_owned(), Json::String(kind.to_owned()));
        entry.insert("id".to_owned(), Json::String(id.clone()));
        entry.insert("parentId".to_owned(), std::mem::replace(&mut self.parent, Json::String(id)));
        entry.insert("timestamp".to_owned(), Json::String(time::format_iso(ts)));
        for (name, value) in fields {
            entry.insert(name.to_owned(), value);
        }
        self.entries.push(entry);
    }

    fn compaction(&mut self, node: &Node) {
        let ts = node.int_attr(attr::CREATED).unwrap_or(0);
        self.emit(
            node.id,
            ts,
            "compaction",
            vec![
                ("summary", Json::String(node.text.clone())),
                ("tokensBefore", Json::from(node.int_attr(attr::TOKENS_BEFORE).unwrap_or(0))),
            ],
        );
        self.first_kept.push(self.entries.len() - 1);
    }

    /// Writes a live turn: user messages, then each run of assistant messages
    /// and calls as one assistant message followed by the calls' results.
    fn turn(&mut self, dom: &Dom, turn: &Node) {
        let ts = turn.int_attr(attr::STARTED).unwrap_or(0);
        let mut group: Vec<&Node> = Vec::new();
        for item in dom.children(turn.id) {
            match (item.kind, item.str_attr(attr::ROLE).and_then(Role::parse)) {
                (ElementKind::Message, Some(Role::User)) => {
                    self.assistant(dom, turn.id, &mut group, ts);
                    let message = json!({
                        "role": "user",
                        "content": [{ "type": "text", "text": item.text }],
                        "timestamp": ts,
                    });
                    self.emit(item.id, ts, "message", vec![("message", message)]);
                }
                (ElementKind::Message, Some(Role::Assistant)) | (ElementKind::Call, _) => group.push(item),
                _ => {}
            }
        }
        self.assistant(dom, turn.id, &mut group, ts);
    }

    /// Writes the pending assistant messages and calls, then the call results.
    fn assistant(&mut self, dom: &Dom, turn: NodeId, group: &mut Vec<&Node>, ts: i64) {
        let Some(first) = group.first() else { return };
        let first = first.id;
        let mut content = Vec::with_capacity(group.len());
        let mut model = None;
        let mut has_call = false;
        for item in group.iter() {
            if item.kind == ElementKind::Call {
                has_call = true;
                let arguments = dom
                    .children_of_kind(item.id, ElementKind::Input)
                    .next()
                    .and_then(|input| serde_json::from_str::<Json>(&input.text).ok())
                    .filter(Json::is_object)
                    .unwrap_or_else(|| json!({}));
                let mut block = json!({
                    "type": "toolCall",
                    "id": item.str_attr(attr::CALL_ID).unwrap_or_default(),
                    "name": item.str_attr(attr::TOOL).unwrap_or_default(),
                    "arguments": arguments,
                });
                if let (Some(intent), Json::Object(fields)) = (item.str_attr(attr::TITLE), &mut block) {
                    fields.insert("intent".to_owned(), Json::String(intent.to_owned()));
                }
                content.push(block);
                continue;
            }
            model = model.or(item.str_attr(attr::MODEL));
            let thinking = item.str_attr(attr::KIND).and_then(MessageKind::parse) == Some(MessageKind::Thinking);
            content.push(if thinking {
                json!({ "type": "thinking", "thinking": item.text })
            } else {
                json!({ "type": "text", "text": item.text })
            });
        }
        let (provider, model) = match model.and_then(|model| model.split_once('/')) {
            Some((provider, model)) => (provider, model),
            None => ("unknown", model.unwrap_or("unknown")),
        };
        let usage = group.iter()
            .filter(|item| item.kind == ElementKind::Message)
            .find_map(|item| dom.children_of_kind(item.id, ElementKind::Usage).next())
            .or_else(|| dom.children_of_kind(turn, ElementKind::Usage).next());
        let count = |name| usage.and_then(|usage| usage.int_attr(name)).unwrap_or(0);
        let cost = usage.and_then(|usage| usage.attr(attr::COST)).and_then(loa_core::Value::as_float).unwrap_or(0.0);
        let stop_reason = if has_call {
            "toolUse"
        } else {
            match dom.get(turn).and_then(|turn| turn.str_attr(attr::STATUS)) {
                Some("error") => "error",
                Some("cancelled") => "aborted",
                _ => "stop",
            }
        };
        let message = json!({
            "role": "assistant",
            "content": content,
            "api": "unknown",
            "provider": provider,
            "model": model,
            "usage": {
                "input": count(attr::INPUT_TOKENS),
                "output": count(attr::OUTPUT_TOKENS),
                "cacheRead": count(attr::CACHE_READ_TOKENS),
                "cacheWrite": count(attr::CACHE_WRITE_TOKENS),
                "totalTokens": count(attr::INPUT_TOKENS) + count(attr::OUTPUT_TOKENS),
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": cost },
            },
            "stopReason": stop_reason,
            "timestamp": ts,
        });
        self.emit(first, ts, "message", vec![("message", message)]);

        for call in group.drain(..).filter(|item| item.kind == ElementKind::Call) {
            let Some(result) = dom.children_of_kind(call.id, ElementKind::Result).next() else { continue };
            let message = json!({
                "role": "toolResult",
                "toolCallId": call.str_attr(attr::CALL_ID).unwrap_or_default(),
                "toolName": call.str_attr(attr::TOOL).unwrap_or_default(),
                "content": [{ "type": "text", "text": result.text }],
                "isError": result.bool_attr(attr::IS_ERROR).unwrap_or(false),
                "timestamp": ts,
            });
            self.emit(result.id, ts, "message", vec![("message", message)]);
        }
    }
}
