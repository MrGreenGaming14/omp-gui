//! omp JSONL session file to DOM.

use super::{HARNESS, X_ENTRY, X_LINE, time, to_line};
use loa_adapter::AdapterError;
use loa_core::{
    CallStatus, Draft, ElementKind, MessageKind, MessageStatus, NodeId, PatchSink, Role, SCHEMA_VERSION, SessionMode,
    TurnStatus, Value, attr,
};
use serde_json::{Map, Value as Json};
use std::collections::{HashMap, HashSet};

/// Session file version this module reads and writes.
const VERSION: u64 = 3;

/// One non-blank, valid JSON object line of the file.
struct Line {
    /// Position among the file's valid lines.
    line: i64,
    /// The line as it goes back out: verbatim, or re-serialized after a migration.
    raw: String,
    json: Map<String, Json>,
    /// A migration changed `json`, so `raw` must be rebuilt from it.
    changed: bool,
    /// Unix milliseconds of the entry.
    ts: i64,
}

pub(super) fn run(text: &str, sink: &dyn PatchSink, node: &str) -> Result<(), AdapterError> {
    let mut lines = parse_lines(text).into_iter();
    let no_header = || AdapterError::Format("the file has no session header".to_owned());
    let mut first = lines.next().ok_or_else(no_header)?;
    let mut title_slot = None;
    if str_field(&first.json, "type") == Some("title") {
        title_slot = Some(first);
        first = lines.next().ok_or_else(no_header)?;
    }
    let mut header = first;
    if str_field(&header.json, "type") != Some("session") || str_field(&header.json, "id").is_none() {
        return Err(AdapterError::Format(format!("line {} isn't a session header", header.line)));
    }
    let mut entries: Vec<Line> = lines.collect();

    migrate(&mut header, &mut entries);
    for line in std::iter::once(&mut header).chain(entries.iter_mut()) {
        if line.changed {
            line.raw = to_line(&line.json)?;
        }
    }

    header.ts = str_field(&header.json, "timestamp").and_then(time::parse_iso).unwrap_or(0);
    let mut previous = header.ts;
    for entry in &mut entries {
        entry.ts = str_field(&entry.json, "timestamp").and_then(time::parse_iso).unwrap_or(previous);
        previous = entry.ts;
    }

    let title = title_slot
        .as_ref()
        .and_then(|slot| str_field(&slot.json, "title"))
        .or_else(|| str_field(&header.json, "title"));
    let id = str_field(&header.json, "id").unwrap_or_default();
    let mut attrs: Vec<(&str, Value)> = vec![
        (attr::ID, id.into()),
        (attr::HARNESS, HARNESS.into()),
        (attr::NODE, node.into()),
        (attr::CREATED, header.ts.into()),
        (attr::SCHEMA, Value::Int(i64::from(SCHEMA_VERSION))),
        (attr::NATIVE_ID, id.into()),
    ];
    if let Some(cwd) = str_field(&header.json, "cwd") {
        attrs.push((attr::CWD, cwd.into()));
    }
    if let Some(title) = title {
        attrs.push((attr::TITLE, title.into()));
    }
    attrs.push((X_ENTRY, header.raw.as_str().into()));
    attrs.push((X_LINE, header.line.into()));
    let session = sink.commit_at(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""), header.ts)?.node;

    let mut importer = Importer { sink, session, turn: None, turns: Vec::new() };
    if let Some(slot) = &title_slot {
        importer.insert(session, ElementKind::Ext, ext_attrs("title", slot), slot.raw.as_str(), header.ts)?;
    }

    let path = active_path(&entries);
    let mut on_path = vec![false; entries.len()];
    for &index in &path {
        if let Some(entry) = entries.get(index) {
            on_path[index] = true;
            importer.entry(entry)?;
        }
    }
    importer.close_turn()?;
    for (entry, _) in entries.iter().zip(&on_path).filter(|(_, on)| !**on) {
        importer.insert(session, ElementKind::Ext, ext_attrs("branch", entry), entry.raw.as_str(), entry.ts)?;
    }
    Ok(())
}

/// Every non-blank line that parses as a JSON object. Malformed lines are
/// skipped, as omp's lenient loader does.
fn parse_lines(text: &str) -> Vec<Line> {
    text.lines()
        .filter(|raw| !raw.trim().is_empty())
        .filter_map(|raw| match serde_json::from_str::<Json>(raw) {
            Ok(Json::Object(json)) => Some((raw, json)),
            _ => None,
        })
        .enumerate()
        .map(|(index, (raw, json))| Line {
            line: i64::try_from(index).unwrap_or(i64::MAX),
            raw: raw.to_owned(),
            json,
            changed: false,
            ts: 0,
        })
        .collect()
}

/// Brings a file to version 3 the way omp's session migrations do.
///
/// v1 entries get ids from their position among the logical entries (header
/// at 0, so the first entry is `00000001`), a linear parent chain, and
/// compaction `firstKeptEntryIndex` (which counts the header) becomes
/// `firstKeptEntryId`. v2 `hookMessage` roles become `custom`.
fn migrate(header: &mut Line, entries: &mut [Line]) {
    let version = header.json.get("version").and_then(Json::as_f64);
    if version.is_some_and(|v| v >= VERSION as f64) {
        return;
    }
    if version.is_none_or(|v| v < 2.0) {
        let ids: Vec<String> = (1..=entries.len()).map(|i| format!("{i:08x}")).collect();
        let mut parent = Json::Null;
        for (entry, id) in entries.iter_mut().zip(&ids) {
            entry.json.insert("id".to_owned(), Json::String(id.clone()));
            entry.json.insert("parentId".to_owned(), parent);
            parent = Json::String(id.clone());
            let kept_index = (str_field(&entry.json, "type") == Some("compaction"))
                .then(|| entry.json.get("firstKeptEntryIndex"))
                .flatten()
                .and_then(Json::as_u64);
            if let Some(index) = kept_index {
                let kept = usize::try_from(index).ok().and_then(|i| i.checked_sub(1)).and_then(|i| ids.get(i));
                if let Some(kept) = kept {
                    entry.json.insert("firstKeptEntryId".to_owned(), Json::String(kept.clone()));
                }
                entry.json.remove("firstKeptEntryIndex");
            }
            entry.changed = true;
        }
    }
    for entry in entries.iter_mut() {
        if str_field(&entry.json, "type") != Some("message") {
            continue;
        }
        if let Some(Json::Object(message)) = entry.json.get_mut("message")
            && message.get("role").and_then(Json::as_str) == Some("hookMessage")
        {
            message.insert("role".to_owned(), Json::String("custom".to_owned()));
            entry.changed = true;
        }
    }
    header.json.insert("version".to_owned(), Json::from(VERSION));
    header.changed = true;
}

/// Indices of the entries from the root to the leaf (the last entry in the
/// file), following `parentId` and stopping on a repeated id.
fn active_path(entries: &[Line]) -> Vec<usize> {
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if let Some(id) = str_field(&entry.json, "id") {
            by_id.insert(id, index);
        }
    }
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    let mut current = entries.len().checked_sub(1);
    while let Some(index) = current {
        let Some(entry) = entries.get(index) else { break };
        if let Some(id) = str_field(&entry.json, "id")
            && !seen.insert(id)
        {
            break;
        }
        path.push(index);
        current = str_field(&entry.json, "parentId").and_then(|parent| by_id.get(parent).copied());
    }
    path.reverse();
    path
}

/// The turn entries are currently added to.
struct OpenTurn {
    node: NodeId,
    /// Timestamp of the last entry placed in the turn: its `ended`.
    last_ts: i64,
    /// Status set at close, from the last assistant `stopReason`.
    status: TurnStatus,
    /// `<call>` nodes by the harness's tool call id.
    calls: HashMap<String, NodeId>,
}

struct Importer<'a> {
    sink: &'a dyn PatchSink,
    session: NodeId,
    turn: Option<OpenTurn>,
    /// Every `<turn>` inserted so far, in order.
    turns: Vec<NodeId>,
}

impl Importer<'_> {
    fn insert(
        &self,
        parent: NodeId,
        kind: ElementKind,
        attrs: Vec<(&str, Value)>,
        text: impl Into<String>,
        ts: i64,
    ) -> Result<NodeId, AdapterError> {
        Ok(self.sink.commit_at(Draft::insert(parent, kind, attrs, text), ts)?.node)
    }

    fn set(&self, node: NodeId, name: &str, value: impl Into<Value>, ts: i64) -> Result<(), AdapterError> {
        self.sink.commit_at(Draft::set(node, name, value), ts)?;
        Ok(())
    }

    /// Maps one entry of the active path.
    fn entry(&mut self, entry: &Line) -> Result<(), AdapterError> {
        let json = &entry.json;
        let ts = entry.ts;
        match str_field(json, "type").unwrap_or_default() {
            "message" => self.message(entry),
            "compaction" => self.compaction(entry),
            "model_change" => {
                if let Some(model) = str_field(json, "model")
                    && matches!(str_field(json, "role"), None | Some("default"))
                {
                    self.set(self.session, attr::MODEL, model, ts)?;
                }
                self.ext(entry, "model_change")
            }
            "thinking_level_change" => {
                if let Some(level) = str_field(json, "thinkingLevel") {
                    self.set(self.session, attr::EFFORT, level, ts)?;
                }
                self.ext(entry, "thinking_level_change")
            }
            "mode_change" => {
                let mode = match str_field(json, "mode") {
                    Some("plan") => SessionMode::Plan,
                    _ => SessionMode::Normal,
                };
                self.set(self.session, attr::MODE, mode, ts)?;
                self.ext(entry, "mode_change")
            }
            "title_change" => {
                if let Some(title) = str_field(json, "title") {
                    self.set(self.session, attr::TITLE, title, ts)?;
                }
                self.ext(entry, "title_change")
            }
            other => self.ext(entry, other),
        }
    }

    fn message(&mut self, entry: &Line) -> Result<(), AdapterError> {
        let Some(Json::Object(message)) = entry.json.get("message") else {
            return self.ext(entry, "message");
        };
        let role = str_field(message, "role").unwrap_or_default();
        match role {
            "user"
                if message.get("synthetic").and_then(Json::as_bool) != Some(true)
                    && str_field(message, "attribution") != Some("agent") =>
            {
                self.user(entry, message)
            }
            "assistant" => self.assistant(entry, message),
            "toolResult" => self.tool_result(entry, message),
            _ => self.ext(entry, &format!("message:{role}")),
        }
    }

    fn user(&mut self, entry: &Line, message: &Map<String, Json>) -> Result<(), AdapterError> {
        self.close_turn()?;
        let ts = entry.ts;
        let id = str_field(&entry.json, "id");
        let mut attrs: Vec<(&str, Value)> = Vec::new();
        if let Some(id) = id {
            attrs.push((attr::ID, id.into()));
        }
        attrs.push((attr::STATUS, TurnStatus::Running.into()));
        attrs.push((attr::STARTED, ts.into()));
        let turn = self.insert(self.session, ElementKind::Turn, attrs, "", ts)?;
        self.turns.push(turn);
        self.turn = Some(OpenTurn { node: turn, last_ts: ts, status: TurnStatus::Done, calls: HashMap::new() });

        let mut attrs: Vec<(&str, Value)> =
            vec![(attr::ROLE, Role::User.into()), (attr::STATUS, MessageStatus::Done.into())];
        if let Some(id) = id {
            attrs.push((attr::ID, id.into()));
        }
        mark(&mut attrs, entry);
        self.insert(turn, ElementKind::Message, attrs, content_text(message.get("content")), ts)?;
        Ok(())
    }

    fn assistant(&mut self, entry: &Line, message: &Map<String, Json>) -> Result<(), AdapterError> {
        let ts = entry.ts;
        let turn = self.open_turn(ts)?;
        let model = match (str_field(message, "provider"), str_field(message, "model")) {
            (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
            (provider, model) => model.or(provider).map(str::to_owned),
        };
        let mut marked = false;
        let mut last_message = None;
        let blocks = message.get("content").and_then(Json::as_array).map_or(&[][..], Vec::as_slice);
        for block in blocks.iter().filter_map(Json::as_object) {
            match str_field(block, "type") {
                Some(kind @ ("text" | "thinking")) => {
                    let thinking = kind == "thinking";
                    let mut attrs: Vec<(&str, Value)> =
                        vec![(attr::ROLE, Role::Assistant.into()), (attr::STATUS, MessageStatus::Done.into())];
                    if thinking {
                        attrs.push((attr::KIND, MessageKind::Thinking.into()));
                    }
                    if let Some(model) = &model {
                        attrs.push((attr::MODEL, model.as_str().into()));
                    }
                    if !marked {
                        mark(&mut attrs, entry);
                        marked = true;
                    }
                    let text = str_field(block, if thinking { "thinking" } else { "text" }).unwrap_or_default();
                    last_message = Some(self.insert(turn, ElementKind::Message, attrs, text, ts)?);
                }
                Some("toolCall") => {
                    let call_id = str_field(block, "id");
                    let mut attrs: Vec<(&str, Value)> = vec![
                        (attr::TOOL, str_field(block, "name").unwrap_or_default().into()),
                        (attr::STATUS, CallStatus::Running.into()),
                        (attr::STARTED, ts.into()),
                    ];
                    if let Some(call_id) = call_id {
                        attrs.push((attr::CALL_ID, call_id.into()));
                    }
                    if let Some(intent) = str_field(block, "intent") {
                        attrs.push((attr::TITLE, intent.into()));
                    }
                    if !marked {
                        mark(&mut attrs, entry);
                        marked = true;
                    }
                    let call = self.insert(turn, ElementKind::Call, attrs, "", ts)?;
                    let input = match block.get("arguments") {
                        Some(arguments) => to_line(arguments)?,
                        None => "{}".to_owned(),
                    };
                    self.insert(call, ElementKind::Input, vec![(attr::FORMAT, "json".into())], input, ts)?;
                    if let (Some(call_id), Some(open)) = (call_id, self.turn.as_mut()) {
                        open.calls.insert(call_id.to_owned(), call);
                    }
                }
                _ => {}
            }
        }
        if !marked {
            self.ext(entry, "message")?;
        }
        if let Some(usage) = message.get("usage").and_then(Json::as_object) {
            let target = last_message.unwrap_or(turn);
            let mut attrs: Vec<(&str, Value)> = Vec::new();
            for (name, key) in [
                (attr::INPUT_TOKENS, "input"),
                (attr::OUTPUT_TOKENS, "output"),
                (attr::CACHE_READ_TOKENS, "cacheRead"),
                (attr::CACHE_WRITE_TOKENS, "cacheWrite"),
            ] {
                if let Some(count) = usage.get(key).and_then(Json::as_i64) {
                    attrs.push((name, count.into()));
                }
            }
            if let Some(cost) = usage.get("cost").and_then(|cost| cost.get("total")).and_then(Json::as_f64) {
                attrs.push((attr::COST, cost.into()));
            }
            if let Some(model) = &model {
                attrs.push((attr::MODEL, model.as_str().into()));
            }
            self.insert(target, ElementKind::Usage, attrs, "", ts)?;
        }
        if let Some(open) = self.turn.as_mut() {
            open.last_ts = ts;
            open.status = match str_field(message, "stopReason") {
                Some("error") => TurnStatus::Error,
                Some("aborted") => TurnStatus::Cancelled,
                _ => TurnStatus::Done,
            };
        }
        Ok(())
    }

    fn tool_result(&mut self, entry: &Line, message: &Map<String, Json>) -> Result<(), AdapterError> {
        let call = self
            .turn
            .as_ref()
            .zip(str_field(message, "toolCallId"))
            .and_then(|(open, id)| open.calls.get(id).copied());
        let Some(call) = call else {
            return self.ext(entry, "message");
        };
        let ts = entry.ts;
        let is_error = message.get("isError").and_then(Json::as_bool).unwrap_or(false);
        let mut attrs: Vec<(&str, Value)> = vec![(attr::IS_ERROR, is_error.into())];
        mark(&mut attrs, entry);
        self.insert(call, ElementKind::Result, attrs, content_text(message.get("content")), ts)?;
        let status = if is_error { CallStatus::Error } else { CallStatus::Done };
        self.set(call, attr::STATUS, status, ts)?;
        self.set(call, attr::ENDED, ts, ts)?;
        if let Some(open) = self.turn.as_mut() {
            open.last_ts = ts;
        }
        Ok(())
    }

    fn compaction(&mut self, entry: &Line) -> Result<(), AdapterError> {
        let covers: Vec<Value> =
            self.turns.iter().map(|turn| Value::Int(i64::try_from(turn.0).unwrap_or(i64::MAX))).collect();
        let mut attrs: Vec<(&str, Value)> = vec![(attr::COVERS, covers.into())];
        if let Some(tokens) = entry.json.get("tokensBefore").and_then(Json::as_i64) {
            attrs.push((attr::TOKENS_BEFORE, tokens.into()));
        }
        attrs.push((attr::CREATED, entry.ts.into()));
        mark(&mut attrs, entry);
        let summary = str_field(&entry.json, "summary").unwrap_or_default();
        self.insert(self.session, ElementKind::Compaction, attrs, summary, entry.ts)?;
        Ok(())
    }

    /// Keeps an entry as `<ext type>` under the open turn, or the session.
    fn ext(&mut self, entry: &Line, kind: &str) -> Result<(), AdapterError> {
        let parent = match self.turn.as_mut() {
            Some(open) => {
                open.last_ts = entry.ts;
                open.node
            }
            None => self.session,
        };
        self.insert(parent, ElementKind::Ext, ext_attrs(kind, entry), entry.raw.as_str(), entry.ts)?;
        Ok(())
    }

    /// The open turn, starting one without a user message when none is open.
    fn open_turn(&mut self, ts: i64) -> Result<NodeId, AdapterError> {
        if let Some(open) = &self.turn {
            return Ok(open.node);
        }
        let attrs: Vec<(&str, Value)> = vec![(attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, ts.into())];
        let turn = self.insert(self.session, ElementKind::Turn, attrs, "", ts)?;
        self.turns.push(turn);
        self.turn = Some(OpenTurn { node: turn, last_ts: ts, status: TurnStatus::Done, calls: HashMap::new() });
        Ok(turn)
    }

    fn close_turn(&mut self) -> Result<(), AdapterError> {
        if let Some(open) = self.turn.take() {
            self.set(open.node, attr::STATUS, open.status, open.last_ts)?;
            self.set(open.node, attr::ENDED, open.last_ts, open.last_ts)?;
        }
        Ok(())
    }
}

/// Attributes of an `<ext>` that keeps `line` as its text.
fn ext_attrs(kind: &str, line: &Line) -> Vec<(&'static str, Value)> {
    vec![(attr::HARNESS, HARNESS.into()), (attr::TYPE, kind.into()), (X_LINE, line.line.into())]
}

/// Adds the entry's raw line and position to an element's attributes.
fn mark(attrs: &mut Vec<(&str, Value)>, entry: &Line) {
    attrs.push((X_ENTRY, entry.raw.as_str().into()));
    attrs.push((X_LINE, entry.line.into()));
}

/// Text of a message `content`: the string, or the text blocks joined by newlines.
fn content_text(content: Option<&Json>) -> String {
    match content {
        Some(Json::String(text)) => text.clone(),
        Some(Json::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Json::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Json::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn str_field<'a>(map: &'a Map<String, Json>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Json::as_str)
}
