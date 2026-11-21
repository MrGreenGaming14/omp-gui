//! Import and export of Codex rollout files (`~/.codex/sessions/**/rollout-*.jsonl`).
//!
//! A rollout is one JSON object per line: `session_meta`, `response_item`
//! (messages, reasoning, tool calls and their outputs), `event_msg`,
//! `turn_context`, `compacted`, and whatever newer Codex versions add.
//!
//! Every line maps to exactly one DOM node, which records the line number in
//! `x-line`. Lines with a DOM shape become that element; the fields the
//! element holds (ids, text, arguments, output) are removed from the line and
//! the rest is kept as JSON in `x-codex`. Lines with no DOM shape become
//! `<ext harness="codex">` holding the raw line. Export puts the fields back,
//! orders the lines by `x-line`, and writes nodes that came from a live session
//! (no `x-line`) in the shapes Codex itself writes, so the file reproduces
//! structurally.

use crate::codex::HARNESS;
use crate::codex::json::{str_at, take_str};
use loa_util::time::{format_iso_ms, parse_iso_ms};
use loa_contracts::harness::{AdapterError, Result};
use loa_core::{
    CallStatus, Dom, Draft, ElementKind, MessageKind, MessageStatus, Node, NodeId, PatchSink, Role, SCHEMA_VERSION,
    SessionMode, TurnStatus, Value, attr,
};
use serde_json::{Map, Value as Json, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};

/// Line number (0 based) of the rollout line a node came from.
pub const X_LINE: &str = "x-line";
/// The rollout line minus the fields the node holds, as JSON.
pub const X_CODEX: &str = "x-codex";
/// Byte lengths of the text parts joined into the node text.
pub const X_PARTS: &str = "x-codex-parts";
/// Line number of the `task_complete` or `turn_aborted` line that ended a turn.
pub const X_LINE_END: &str = "x-line-end";
/// The ending line of a turn minus its turn id, as JSON.
pub const X_CODEX_END: &str = "x-codex-end";
/// A turn the importer opened because the file had no `task_started` line for it.
pub const X_IMPLICIT: &str = "x-codex-implicit";
/// Top level key on lines this crate writes, for DOM data Codex has no field for.
const LOA_KEY: &str = "x-loa";

const TEXT_PARTS: &[&str] = &["input_text", "output_text", "summary_text", "reasoning_text", "text"];
const MESSAGE_SEPARATOR: &str = "";
const REASONING_SEPARATOR: &str = "\n\n";
/// Starts of user-role messages Codex injects itself (instructions, environment).
const INJECTED_PREFIXES: &[&str] = &[
    "<environment_context>",
    "<user_instructions>",
    "# AGENTS.md instructions",
    "<permissions instructions>",
    "<turn_aborted>",
];

fn format_error(line: usize, detail: impl std::fmt::Display) -> AdapterError {
    AdapterError::Format(format!("line {}: {detail}", line + 1))
}

/// Reads a rollout into `sink`, which holds an empty DOM. `node` is the owner
/// node key written to `<session node>`.
pub fn import(reader: impl BufRead, sink: &dyn PatchSink, node: &str) -> Result<()> {
    let mut importer = Importer::new(sink, node);
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        importer.line(index, &line)?;
    }
    if importer.session.is_none() {
        return Err(AdapterError::Format("the rollout has no session_meta line".to_owned()));
    }
    importer.finish()
}

struct OpenTurn {
    node: NodeId,
    id: Option<String>,
    implicit: bool,
    /// The turn already holds agent output, so a new user prompt starts a new turn.
    replied: bool,
    /// Calls without an output yet, by node.
    open_calls: Vec<NodeId>,
}

struct Importer<'a> {
    sink: &'a dyn PatchSink,
    node: &'a str,
    ts: i64,
    session: Option<NodeId>,
    turn: Option<OpenTurn>,
    calls: HashMap<String, NodeId>,
    turns: Vec<NodeId>,
    covered: usize,
    model: Option<String>,
    effort: Option<String>,
    mode: Option<SessionMode>,
}

/// What a rollout line was turned into.
enum Outcome {
    Done,
    /// The line has no DOM shape in this position; keep it raw.
    Ext,
}

impl<'a> Importer<'a> {
    fn new(sink: &'a dyn PatchSink, node: &'a str) -> Self {
        Self {
            sink,
            node,
            ts: 0,
            session: None,
            turn: None,
            calls: HashMap::new(),
            turns: Vec::new(),
            covered: 0,
            model: None,
            effort: None,
            mode: None,
        }
    }

    fn commit(&self, draft: Draft) -> Result<NodeId> {
        Ok(self.sink.commit_at(draft, self.ts)?.node)
    }

    fn line(&mut self, index: usize, raw: &str) -> Result<()> {
        let value: Json = serde_json::from_str(raw).map_err(|e| format_error(index, e))?;
        let Json::Object(line) = value else {
            return Err(format_error(index, "not a JSON object"));
        };
        if let Some(ts) = line.get("timestamp").and_then(Json::as_str).and_then(parse_iso_ms) {
            self.ts = ts;
        }
        let kind = line.get("type").and_then(Json::as_str).unwrap_or_default().to_owned();
        let payload_type = line.get("payload").and_then(|p| str_at(p, "type")).map(str::to_owned);
        let at = Value::Int(i64::try_from(index).unwrap_or(i64::MAX));
        let Some(session) = self.session else {
            if kind != "session_meta" {
                return Err(format_error(index, "the rollout doesn't start with session_meta"));
            }
            return self.session_meta(index, line, at);
        };
        let loa = line.get(LOA_KEY).cloned();
        let outcome = {
            let at = at.clone();
            match (kind.as_str(), payload_type.as_deref()) {
                ("response_item", Some("message")) => self.message(line, at)?,
                ("response_item", Some("reasoning")) => self.reasoning(line, at)?,
                ("response_item", Some("function_call")) => self.call(line, at, CallLine::Function)?,
                ("response_item", Some("custom_tool_call")) => self.call(line, at, CallLine::Custom)?,
                ("response_item", Some("local_shell_call")) => self.call(line, at, CallLine::LocalShell)?,
                ("response_item", Some("function_call_output" | "custom_tool_call_output")) => {
                    self.output(line, at, loa.as_ref())?
                }
                ("compacted", _) => self.compacted(session, line, at)?,
                ("turn_context", _) => {
                    self.turn_context(session, &line)?;
                    Outcome::Ext
                }
                ("event_msg", Some("task_started" | "turn_started")) => self.task_started(line, at)?,
                ("event_msg", Some("task_complete" | "turn_complete")) => {
                    let status = match loa.as_ref().and_then(|l| str_at(l, "status")) {
                        Some("error") => TurnStatus::Error,
                        _ => TurnStatus::Done,
                    };
                    self.task_end(line, at, status)?
                }
                ("event_msg", Some("turn_aborted")) => self.task_end(line, at, TurnStatus::Cancelled)?,
                ("event_msg", Some("token_count")) => self.token_count(line, at)?,
                _ => Outcome::Ext,
            }
        };
        if let Outcome::Ext = outcome {
            let parent = self.turn.as_ref().map_or(session, |t| t.node);
            let ty = match payload_type {
                Some(sub) => format!("{kind}:{sub}"),
                None => kind,
            };
            self.commit(Draft::insert(
                parent,
                ElementKind::Ext,
                [(attr::HARNESS, Value::from(HARNESS)), (attr::TYPE, Value::from(ty)), (X_LINE, at)],
                raw,
            ))?;
        }
        Ok(())
    }

    fn session_meta(&mut self, index: usize, mut line: Map<String, Json>, at: Value) -> Result<()> {
        let mut payload = take_object(&mut line, "payload").ok_or_else(|| format_error(index, "session_meta without payload"))?;
        let native = take_str(&mut payload, "id").ok_or_else(|| format_error(index, "session_meta without id"))?;
        let cwd = take_str(&mut payload, "cwd");
        let created = payload.get("timestamp").and_then(Json::as_str).and_then(parse_iso_ms).unwrap_or(self.ts);
        line.insert("payload".to_owned(), Json::Object(payload));
        let mut attrs = vec![
            (attr::ID, Value::from(native.as_str())),
            (attr::HARNESS, Value::from(HARNESS)),
            (attr::NODE, Value::from(self.node)),
            (attr::CREATED, Value::Int(created)),
            (attr::SCHEMA, Value::Int(i64::from(SCHEMA_VERSION))),
            (attr::NATIVE_ID, Value::from(native)),
            (X_CODEX, Value::from(Json::Object(line).to_string())),
            (X_LINE, at),
        ];
        if let Some(cwd) = cwd {
            attrs.push((attr::CWD, Value::from(cwd)));
        }
        self.session = Some(self.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""))?);
        Ok(())
    }

    /// The open turn, opening an implicit one when the file has no turn markers.
    fn turn_for(&mut self, prompt: bool) -> Result<&mut OpenTurn> {
        let needs_new = match &self.turn {
            None => true,
            Some(turn) => prompt && turn.implicit && turn.replied,
        };
        if needs_new {
            self.close_turn(TurnStatus::Done)?;
            let session = self.session.ok_or_else(|| AdapterError::Format("no session".to_owned()))?;
            let node = self.commit(Draft::insert(
                session,
                ElementKind::Turn,
                [
                    (attr::STATUS, Value::from(TurnStatus::Running)),
                    (attr::STARTED, Value::Int(self.ts)),
                    (X_IMPLICIT, Value::Bool(true)),
                ],
                "",
            ))?;
            self.turns.push(node);
            self.turn = Some(OpenTurn { node, id: None, implicit: true, replied: false, open_calls: Vec::new() });
        }
        self.turn.as_mut().ok_or_else(|| AdapterError::Format("no turn".to_owned()))
    }

    /// Ends the open turn: calls still waiting for output are cancelled, the
    /// turn gets `status` unless it is implicit (always `done`).
    fn close_turn(&mut self, status: TurnStatus) -> Result<()> {
        let Some(turn) = self.turn.take() else { return Ok(()) };
        for call in &turn.open_calls {
            self.commit(Draft::set(*call, attr::STATUS, CallStatus::Cancelled))?;
        }
        let status = if turn.implicit { TurnStatus::Done } else { status };
        self.commit(Draft::set(turn.node, attr::STATUS, status))?;
        Ok(())
    }

    fn message(&mut self, mut line: Map<String, Json>, at: Value) -> Result<Outcome> {
        let Some(mut payload) = take_object(&mut line, "payload") else { return Ok(Outcome::Ext) };
        let mut text = String::new();
        let mut lengths = Vec::new();
        let injected = payload.get("content").and_then(Json::as_array).is_some_and(|parts| {
            let mut texts = parts.iter().filter_map(|p| str_at(p, "text")).peekable();
            texts.peek().is_some() && texts.all(|t| INJECTED_PREFIXES.iter().any(|prefix| t.starts_with(prefix)))
        });
        if let Some(Json::Array(parts)) = payload.get_mut("content") {
            take_parts(parts, MESSAGE_SEPARATOR, &mut text, &mut lengths);
        }
        let role = match payload.get("role").and_then(Json::as_str) {
            Some("assistant") => Role::Assistant,
            Some("user") if !injected => Role::User,
            _ => Role::System,
        };
        let id = take_str(&mut payload, "id");
        line.insert("payload".to_owned(), Json::Object(payload));
        let turn = self.turn_for(role == Role::User)?;
        if role == Role::Assistant {
            turn.replied = true;
        }
        let parent = turn.node;
        let mut attrs = vec![
            (attr::ROLE, Value::from(role)),
            (attr::STATUS, Value::from(MessageStatus::Done)),
            (X_CODEX, Value::from(Json::Object(line).to_string())),
            (X_PARTS, Value::List(lengths)),
            (X_LINE, at),
        ];
        if let Some(id) = id {
            attrs.push((attr::ID, Value::from(id)));
        }
        self.commit(Draft::insert(parent, ElementKind::Message, attrs, text))?;
        Ok(Outcome::Done)
    }

    fn reasoning(&mut self, mut line: Map<String, Json>, at: Value) -> Result<Outcome> {
        let Some(mut payload) = take_object(&mut line, "payload") else { return Ok(Outcome::Ext) };
        let mut text = String::new();
        let mut lengths = Vec::new();
        for key in ["summary", "content"] {
            if let Some(Json::Array(parts)) = payload.get_mut(key) {
                take_parts(parts, REASONING_SEPARATOR, &mut text, &mut lengths);
            }
        }
        let id = take_str(&mut payload, "id");
        line.insert("payload".to_owned(), Json::Object(payload));
        let turn = self.turn_for(false)?;
        turn.replied = true;
        let parent = turn.node;
        let mut attrs = vec![
            (attr::ROLE, Value::from(Role::Assistant)),
            (attr::KIND, Value::from(MessageKind::Thinking)),
            (attr::STATUS, Value::from(MessageStatus::Done)),
            (X_CODEX, Value::from(Json::Object(line).to_string())),
            (X_PARTS, Value::List(lengths)),
            (X_LINE, at),
        ];
        if let Some(id) = id {
            attrs.push((attr::ID, Value::from(id)));
        }
        self.commit(Draft::insert(parent, ElementKind::Message, attrs, text))?;
        Ok(Outcome::Done)
    }

    fn call(&mut self, mut line: Map<String, Json>, at: Value, kind: CallLine) -> Result<Outcome> {
        let Some(mut payload) = take_object(&mut line, "payload") else { return Ok(Outcome::Ext) };
        let (tool, input, format, title) = match kind {
            CallLine::Function | CallLine::Custom => {
                let Some(Json::String(_)) = payload.get("name") else {
                    line.insert("payload".to_owned(), Json::Object(payload));
                    return Ok(Outcome::Ext);
                };
                let tool = take_str(&mut payload, "name").unwrap_or_default();
                let field = if kind == CallLine::Function { "arguments" } else { "input" };
                let input = take_str(&mut payload, field);
                (tool, input, (kind == CallLine::Custom).then_some("text"), None)
            }
            CallLine::LocalShell => {
                let action = payload.remove("action");
                let title = action.as_ref().and_then(|a| a.get("command")).and_then(Json::as_array).map(|parts| {
                    parts.iter().filter_map(Json::as_str).collect::<Vec<_>>().join(" ")
                });
                ("local_shell".to_owned(), action.map(|a| a.to_string()), None, title)
            }
        };
        let call_id = take_str(&mut payload, "call_id");
        let key = call_id.clone().or_else(|| payload.get("id").and_then(Json::as_str).map(str::to_owned));
        line.insert("payload".to_owned(), Json::Object(payload));
        let turn = self.turn_for(false)?;
        turn.replied = true;
        let parent = turn.node;
        let mut attrs = vec![
            (attr::TOOL, Value::from(tool)),
            (attr::STATUS, Value::from(CallStatus::Running)),
            (X_CODEX, Value::from(Json::Object(line).to_string())),
            (X_LINE, at),
        ];
        if let Some(call_id) = call_id {
            attrs.push((attr::CALL_ID, Value::from(call_id)));
        }
        if let Some(title) = title {
            attrs.push((attr::TITLE, Value::from(title)));
        }
        let call = self.commit(Draft::insert(parent, ElementKind::Call, attrs, ""))?;
        if let Some(input) = input {
            let attrs: Vec<(&str, Value)> = format.map(|f| (attr::FORMAT, Value::from(f))).into_iter().collect();
            self.commit(Draft::insert(call, ElementKind::Input, attrs, input))?;
        }
        if let Some(key) = key {
            self.calls.insert(key, call);
        }
        if let Some(turn) = self.turn.as_mut() {
            turn.open_calls.push(call);
        }
        Ok(Outcome::Done)
    }

    fn output(&mut self, mut line: Map<String, Json>, at: Value, loa: Option<&Json>) -> Result<Outcome> {
        let Some(call) = line.get("payload").and_then(|p| str_at(p, "call_id")).and_then(|id| self.calls.get(id)).copied()
        else {
            return Ok(Outcome::Ext);
        };
        let Some(mut payload) = take_object(&mut line, "payload") else { return Ok(Outcome::Ext) };
        payload.remove("call_id");
        let mut text = String::new();
        let mut lengths = None;
        match payload.get_mut("output") {
            Some(Json::String(_)) => text = take_str(&mut payload, "output").unwrap_or_default(),
            Some(Json::Array(parts)) => {
                let mut found = Vec::new();
                take_parts(parts, MESSAGE_SEPARATOR, &mut text, &mut found);
                lengths = Some(found);
            }
            Some(other) => text = other.to_string(),
            None => {}
        }
        line.insert("payload".to_owned(), Json::Object(payload));
        let is_error = loa.and_then(|l| l.get("is_error")).and_then(Json::as_bool).unwrap_or(false);
        let mut attrs = vec![(X_CODEX, Value::from(Json::Object(line).to_string())), (X_LINE, at)];
        if let Some(lengths) = lengths {
            attrs.push((X_PARTS, Value::List(lengths)));
        }
        if is_error {
            attrs.push((attr::IS_ERROR, Value::Bool(true)));
        }
        self.commit(Draft::insert(call, ElementKind::Result, attrs, text))?;
        let status = if is_error { CallStatus::Error } else { CallStatus::Done };
        self.commit(Draft::set(call, attr::STATUS, status))?;
        self.commit(Draft::set(call, attr::ENDED, Value::Int(self.ts)))?;
        for turn in self.turn.iter_mut() {
            turn.open_calls.retain(|c| *c != call);
        }
        Ok(Outcome::Done)
    }

    fn compacted(&mut self, session: NodeId, mut line: Map<String, Json>, at: Value) -> Result<Outcome> {
        let Some(mut payload) = take_object(&mut line, "payload") else { return Ok(Outcome::Ext) };
        let summary = take_str(&mut payload, "message").unwrap_or_default();
        line.insert("payload".to_owned(), Json::Object(payload));
        let covers: Vec<Value> = self.turns[self.covered..].iter().map(|t| Value::Int(node_int(*t))).collect();
        self.covered = self.turns.len();
        self.commit(Draft::insert(
            session,
            ElementKind::Compaction,
            [
                (attr::COVERS, Value::List(covers)),
                (attr::CREATED, Value::Int(self.ts)),
                (X_CODEX, Value::from(Json::Object(line).to_string())),
                (X_LINE, at),
            ],
            summary,
        ))?;
        Ok(Outcome::Done)
    }

    fn turn_context(&mut self, session: NodeId, line: &Map<String, Json>) -> Result<()> {
        let Some(payload) = line.get("payload") else { return Ok(()) };
        if let Some(model) = str_at(payload, "model")
            && self.model.as_deref() != Some(model)
        {
            self.model = Some(model.to_owned());
            self.commit(Draft::set(session, attr::MODEL, model))?;
        }
        if let Some(effort) = str_at(payload, "effort")
            && self.effort.as_deref() != Some(effort)
        {
            self.effort = Some(effort.to_owned());
            self.commit(Draft::set(session, attr::EFFORT, effort))?;
        }
        if let Some(mode) = payload.get("collaboration_mode").and_then(|m| str_at(m, "mode")) {
            let mode = if mode == "plan" { SessionMode::Plan } else { SessionMode::Normal };
            if self.mode != Some(mode) {
                self.mode = Some(mode);
                self.commit(Draft::set(session, attr::MODE, mode))?;
            }
        }
        Ok(())
    }

    fn task_started(&mut self, mut line: Map<String, Json>, at: Value) -> Result<Outcome> {
        let Some(mut payload) = take_object(&mut line, "payload") else { return Ok(Outcome::Ext) };
        let id = take_str(&mut payload, "turn_id");
        line.insert("payload".to_owned(), Json::Object(payload));
        self.close_turn(TurnStatus::Cancelled)?;
        let session = self.session.ok_or_else(|| AdapterError::Format("no session".to_owned()))?;
        let mut attrs = vec![
            (attr::STATUS, Value::from(TurnStatus::Running)),
            (attr::STARTED, Value::Int(self.ts)),
            (X_CODEX, Value::from(Json::Object(line).to_string())),
            (X_LINE, at),
        ];
        if let Some(id) = &id {
            attrs.push((attr::ID, Value::from(id.as_str())));
        }
        let node = self.commit(Draft::insert(session, ElementKind::Turn, attrs, ""))?;
        self.turns.push(node);
        self.turn = Some(OpenTurn { node, id, implicit: false, replied: false, open_calls: Vec::new() });
        Ok(Outcome::Done)
    }

    fn task_end(&mut self, mut line: Map<String, Json>, at: Value, status: TurnStatus) -> Result<Outcome> {
        let Some(turn) = &self.turn else { return Ok(Outcome::Ext) };
        let node = turn.node;
        let turn_id = turn.id.clone();
        if let (Some(expected), Some(actual)) = (turn_id.as_deref(), line.get("payload").and_then(|p| str_at(p, "turn_id")))
            && expected != actual
        {
            return Ok(Outcome::Ext);
        }
        if let Some(payload) = line.get_mut("payload").and_then(Json::as_object_mut)
            && turn_id.is_some()
            && payload.get("turn_id").and_then(Json::as_str) == turn_id.as_deref()
        {
            payload.remove("turn_id");
        }
        self.commit(Draft::set(node, X_CODEX_END, Json::Object(line).to_string()))?;
        self.commit(Draft::set(node, X_LINE_END, at))?;
        self.commit(Draft::set(node, attr::ENDED, Value::Int(self.ts)))?;
        self.close_turn(status)?;
        Ok(Outcome::Done)
    }

    fn token_count(&mut self, line: Map<String, Json>, at: Value) -> Result<Outcome> {
        let Some(usage) = line.get("payload").and_then(|p| p.get("info")).and_then(|i| i.get("last_token_usage")) else {
            return Ok(Outcome::Ext);
        };
        let Some(turn) = &self.turn else { return Ok(Outcome::Ext) };
        let mut attrs = vec![(X_CODEX, Value::from(Json::Object(line.clone()).to_string())), (X_LINE, at)];
        for (key, name) in [
            ("input_tokens", attr::INPUT_TOKENS),
            ("output_tokens", attr::OUTPUT_TOKENS),
            ("cached_input_tokens", attr::CACHE_READ_TOKENS),
            ("cache_write_input_tokens", attr::CACHE_WRITE_TOKENS),
        ] {
            if let Some(count) = usage.get(key).and_then(Json::as_i64) {
                attrs.push((name, Value::Int(count)));
            }
        }
        self.commit(Draft::insert(turn.node, ElementKind::Usage, attrs, ""))?;
        Ok(Outcome::Done)
    }

    fn finish(mut self) -> Result<()> {
        self.close_turn(TurnStatus::Cancelled)
    }
}

/// How a call is written in a rollout.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum CallLine {
    Function,
    Custom,
    LocalShell,
}

impl CallLine {
    fn item_type(self) -> &'static str {
        match self {
            CallLine::Function => "function_call",
            CallLine::Custom => "custom_tool_call",
            CallLine::LocalShell => "local_shell_call",
        }
    }

    fn output_type(self) -> &'static str {
        match self {
            CallLine::Custom => "custom_tool_call_output",
            CallLine::Function | CallLine::LocalShell => "function_call_output",
        }
    }
}

fn node_int(id: NodeId) -> i64 {
    i64::try_from(id.0).unwrap_or(i64::MAX)
}

fn take_object(object: &mut Map<String, Json>, key: &str) -> Option<Map<String, Json>> {
    match object.remove(key) {
        Some(Json::Object(inner)) => Some(inner),
        Some(other) => {
            object.insert(key.to_owned(), other);
            None
        }
        None => None,
    }
}

/// Moves the `text` of every text part into `joined` (separated by
/// `separator`) and records each part's byte length in `lengths`.
fn take_parts(parts: &mut [Json], separator: &str, joined: &mut String, lengths: &mut Vec<Value>) {
    for part in parts {
        let Some(object) = part.as_object_mut() else { continue };
        let is_text = object.get("type").and_then(Json::as_str).is_some_and(|t| TEXT_PARTS.contains(&t));
        if !is_text {
            continue;
        }
        if let Some(text) = take_str(object, "text") {
            if !lengths.is_empty() {
                joined.push_str(separator);
            }
            lengths.push(Value::Int(i64::try_from(text.len()).unwrap_or(i64::MAX)));
            joined.push_str(&text);
        }
    }
}

/// Puts `text` back into the text parts emptied by [`take_parts`]. When the
/// text was edited and no longer splits at the recorded lengths, the first
/// part takes all of it.
fn restore_parts(arrays: &mut [&mut Vec<Json>], separator: &str, text: &str, lengths: &[Value]) {
    let mut slots: Vec<&mut Map<String, Json>> = arrays
        .iter_mut()
        .flat_map(|parts| parts.iter_mut())
        .filter_map(Json::as_object_mut)
        .filter(|o| {
            !o.contains_key("text") && o.get("type").and_then(Json::as_str).is_some_and(|t| TEXT_PARTS.contains(&t))
        })
        .collect();
    let split = split_parts(text, separator, lengths).filter(|pieces| pieces.len() == slots.len());
    match split {
        Some(pieces) => {
            for (slot, piece) in slots.iter_mut().zip(pieces) {
                slot.insert("text".to_owned(), Json::from(piece));
            }
        }
        None => {
            for (index, slot) in slots.iter_mut().enumerate() {
                slot.insert("text".to_owned(), Json::from(if index == 0 { text } else { "" }));
            }
        }
    }
}

fn split_parts<'t>(text: &'t str, separator: &str, lengths: &[Value]) -> Option<Vec<&'t str>> {
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

/// Writes `dom` as a rollout.
pub fn export(dom: &Dom, out: &mut impl Write) -> Result<()> {
    let root = dom.root().ok_or_else(|| AdapterError::Format("the DOM is empty".to_owned()))?;
    if root.kind != ElementKind::Session {
        return Err(AdapterError::Format("the DOM root isn't a <session>".to_owned()));
    }
    let mut exporter = Exporter { dom, lines: Vec::new(), key: -1, ts: root.int_attr(attr::CREATED).unwrap_or(0) };
    exporter.node(root)?;
    exporter.lines.sort_by_key(|(key, _)| *key);
    for (_, line) in &exporter.lines {
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

struct Exporter<'d> {
    dom: &'d Dom,
    lines: Vec<(i64, String)>,
    /// Highest line number seen so far; lines from live nodes sort right after it.
    key: i64,
    /// Timestamp for lines generated from live nodes.
    ts: i64,
}

fn residual(node: &Node, name: &str) -> Result<Option<Map<String, Json>>> {
    let Some(text) = node.str_attr(name) else { return Ok(None) };
    match serde_json::from_str(text) {
        Ok(Json::Object(object)) => Ok(Some(object)),
        _ => Err(AdapterError::Format(format!("node {} has a malformed {name}", node.id))),
    }
}

fn line_of(ts: i64, kind: &str, payload: Json) -> Map<String, Json> {
    let mut line = Map::new();
    line.insert("timestamp".to_owned(), Json::from(format_iso_ms(ts)));
    line.insert("type".to_owned(), Json::from(kind));
    line.insert("payload".to_owned(), payload);
    line
}

fn payload_mut(line: &mut Map<String, Json>) -> &mut Map<String, Json> {
    let payload = line.entry("payload").or_insert_with(|| Json::Object(Map::new()));
    if !payload.is_object() {
        *payload = Json::Object(Map::new());
    }
    match payload {
        Json::Object(object) => object,
        // Replaced by an object just above.
        _ => unreachable!("payload was just made an object"),
    }
}

impl Exporter<'_> {
    fn push(&mut self, at: Option<i64>, line: String) {
        let key = match at {
            Some(at) => {
                self.key = self.key.max(at);
                at
            }
            None => self.key,
        };
        self.lines.push((key, line));
    }

    fn children(&mut self, node: &Node) -> Result<()> {
        for child in self.dom.children(node.id) {
            self.node(child)?;
        }
        Ok(())
    }

    fn node(&mut self, node: &Node) -> Result<()> {
        let at = node.int_attr(X_LINE);
        match node.kind {
            ElementKind::Session => {
                self.session(node, at)?;
                self.children(node)
            }
            ElementKind::Turn => self.turn(node, at),
            ElementKind::Message => self.message(node, at),
            ElementKind::Call => self.call(node, at),
            ElementKind::Compaction => self.compaction(node, at),
            ElementKind::Usage => self.usage(node, at),
            ElementKind::Ext => {
                if at.is_some() && node.str_attr(attr::HARNESS) == Some(HARNESS) {
                    self.push(at, node.text.clone());
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn session(&mut self, node: &Node, at: Option<i64>) -> Result<()> {
        let id = node.str_attr(attr::NATIVE_ID).or(node.str_attr(attr::ID)).unwrap_or_default();
        let line = match residual(node, X_CODEX)? {
            Some(mut line) => {
                let payload = payload_mut(&mut line);
                payload.insert("id".to_owned(), Json::from(id));
                if let Some(cwd) = node.str_attr(attr::CWD) {
                    payload.insert("cwd".to_owned(), Json::from(cwd));
                }
                line
            }
            None => line_of(
                self.ts,
                "session_meta",
                json!({
                    "id": id,
                    "session_id": id,
                    "timestamp": format_iso_ms(self.ts),
                    "cwd": node.str_attr(attr::CWD).unwrap_or_default(),
                    "originator": "loa",
                    "cli_version": env!("CARGO_PKG_VERSION"),
                    "source": "cli",
                    "model_provider": null,
                }),
            ),
        };
        self.push(at, Json::Object(line).to_string());
        Ok(())
    }

    fn turn(&mut self, node: &Node, at: Option<i64>) -> Result<()> {
        if let Some(ts) = node.int_attr(attr::STARTED) {
            self.ts = ts;
        }
        let turn_id = node.str_attr(attr::ID).map_or_else(|| format!("loa-turn-{}", node.id), str::to_owned);
        let implicit = node.bool_attr(X_IMPLICIT) == Some(true);
        if !implicit {
            let line = match residual(node, X_CODEX)? {
                Some(mut line) => {
                    if let Some(id) = node.str_attr(attr::ID) {
                        payload_mut(&mut line).insert("turn_id".to_owned(), Json::from(id));
                    }
                    line
                }
                None => line_of(
                    self.ts,
                    "event_msg",
                    json!({ "type": "task_started", "turn_id": turn_id, "model_context_window": null }),
                ),
            };
            self.push(at, Json::Object(line).to_string());
        }
        self.children(node)?;
        if let Some(ts) = node.int_attr(attr::ENDED) {
            self.ts = ts;
        }
        let end_at = node.int_attr(X_LINE_END);
        let end = match residual(node, X_CODEX_END)? {
            Some(mut line) => {
                let payload = payload_mut(&mut line);
                if !payload.contains_key("turn_id")
                    && let Some(id) = node.str_attr(attr::ID)
                {
                    payload.insert("turn_id".to_owned(), Json::from(id));
                }
                if node.str_attr(attr::STATUS) == Some(TurnStatus::Error.as_str()) {
                    line.insert(LOA_KEY.to_owned(), json!({ "status": "error" }));
                }
                Some(line)
            }
            None if implicit => None,
            None => match node.str_attr(attr::STATUS).and_then(TurnStatus::parse) {
                Some(TurnStatus::Done) => Some(line_of(
                    self.ts,
                    "event_msg",
                    json!({ "type": "task_complete", "turn_id": turn_id, "last_agent_message": null }),
                )),
                Some(TurnStatus::Error) => {
                    let mut line = line_of(
                        self.ts,
                        "event_msg",
                        json!({ "type": "task_complete", "turn_id": turn_id, "last_agent_message": null }),
                    );
                    line.insert(LOA_KEY.to_owned(), json!({ "status": "error" }));
                    Some(line)
                }
                Some(TurnStatus::Cancelled) => Some(line_of(
                    self.ts,
                    "event_msg",
                    json!({ "type": "turn_aborted", "turn_id": turn_id, "reason": "interrupted" }),
                )),
                Some(TurnStatus::Running | TurnStatus::Waiting) | None => None,
            },
        };
        if let Some(line) = end {
            self.push(end_at, Json::Object(line).to_string());
        }
        Ok(())
    }

    fn message(&mut self, node: &Node, at: Option<i64>) -> Result<()> {
        let thinking = node.str_attr(attr::KIND) == Some(MessageKind::Thinking.as_str());
        let lengths = node.attr(X_PARTS).and_then(Value::as_list).unwrap_or_default();
        let line = match residual(node, X_CODEX)? {
            Some(mut line) => {
                let payload = payload_mut(&mut line);
                if let Some(id) = node.str_attr(attr::ID) {
                    payload.insert("id".to_owned(), Json::from(id));
                }
                let separator = if thinking { REASONING_SEPARATOR } else { MESSAGE_SEPARATOR };
                let mut arrays = Vec::new();
                let mut summary = take_array(payload, "summary");
                let mut content = take_array(payload, "content");
                if let Some(summary) = summary.as_mut() {
                    arrays.push(summary);
                }
                if let Some(content) = content.as_mut() {
                    arrays.push(content);
                }
                restore_parts(&mut arrays, separator, &node.text, lengths);
                if let Some(summary) = summary {
                    payload.insert("summary".to_owned(), Json::Array(summary));
                }
                if let Some(content) = content {
                    payload.insert("content".to_owned(), Json::Array(content));
                }
                line
            }
            None if thinking => {
                let summary: Vec<Json> = if node.text.is_empty() {
                    Vec::new()
                } else {
                    vec![json!({ "type": "summary_text", "text": node.text })]
                };
                let mut payload = json!({ "type": "reasoning", "summary": summary, "content": null, "encrypted_content": null });
                if let (Some(id), Some(object)) = (node.str_attr(attr::ID), payload.as_object_mut()) {
                    object.insert("id".to_owned(), Json::from(id));
                }
                line_of(self.ts, "response_item", payload)
            }
            None => {
                let (role, part) = match node.str_attr(attr::ROLE).and_then(Role::parse) {
                    Some(Role::Assistant) => ("assistant", "output_text"),
                    Some(Role::User) => ("user", "input_text"),
                    Some(Role::System) | None => ("developer", "input_text"),
                };
                let mut payload = json!({
                    "type": "message",
                    "role": role,
                    "content": [{ "type": part, "text": node.text }],
                });
                if let (Some(id), Some(object)) = (node.str_attr(attr::ID), payload.as_object_mut()) {
                    object.insert("id".to_owned(), Json::from(id));
                }
                line_of(self.ts, "response_item", payload)
            }
        };
        self.push(at, Json::Object(line).to_string());
        Ok(())
    }

    fn call_line(&self, node: &Node) -> Result<CallLine> {
        if let Some(line) = residual(node, X_CODEX)? {
            return Ok(match line.get("payload").and_then(|p| str_at(p, "type")) {
                Some("custom_tool_call") => CallLine::Custom,
                Some("local_shell_call") => CallLine::LocalShell,
                _ => CallLine::Function,
            });
        }
        let input = self.dom.children_of_kind(node.id, ElementKind::Input).next();
        let text_input = input.and_then(|i| i.str_attr(attr::FORMAT)) == Some("text");
        let json_input = input.is_some_and(|i| serde_json::from_str::<Json>(&i.text).is_ok_and(|v| v.is_object()));
        Ok(if text_input {
            CallLine::Custom
        } else if node.str_attr(attr::TOOL) == Some("local_shell") && json_input {
            CallLine::LocalShell
        } else {
            CallLine::Function
        })
    }

    fn call(&mut self, node: &Node, at: Option<i64>) -> Result<()> {
        if let Some(ts) = node.int_attr(attr::STARTED) {
            self.ts = ts;
        }
        let kind = self.call_line(node)?;
        let tool = node.str_attr(attr::TOOL).unwrap_or_default();
        let call_id = node.str_attr(attr::CALL_ID).map_or_else(|| format!("call_loa_{}", node.id), str::to_owned);
        let input = self.dom.children_of_kind(node.id, ElementKind::Input).next().map(|i| i.text.as_str());
        let (mut line, imported) = match residual(node, X_CODEX)? {
            Some(line) => (line, true),
            None => (line_of(self.ts, "response_item", json!({ "type": kind.item_type() })), false),
        };
        let payload = payload_mut(&mut line);
        if !imported || node.attr(attr::CALL_ID).is_some() {
            payload.insert("call_id".to_owned(), Json::from(call_id));
        }
        match kind {
            CallLine::Function => {
                payload.insert("name".to_owned(), Json::from(tool));
                if let Some(input) = input {
                    payload.insert("arguments".to_owned(), Json::from(input));
                } else if !imported {
                    payload.insert("arguments".to_owned(), Json::from("{}"));
                }
            }
            CallLine::Custom => {
                payload.insert("name".to_owned(), Json::from(tool));
                if let Some(input) = input {
                    payload.insert("input".to_owned(), Json::from(input));
                }
                if !imported {
                    payload.insert("status".to_owned(), Json::from("completed"));
                }
            }
            CallLine::LocalShell => {
                if let Some(input) = input {
                    let action = serde_json::from_str(input).unwrap_or_else(|_| Json::from(input));
                    payload.insert("action".to_owned(), action);
                }
                if !imported {
                    payload.insert("status".to_owned(), Json::from("completed"));
                }
            }
        }
        self.push(at, Json::Object(line).to_string());
        if let Some(ts) = node.int_attr(attr::ENDED) {
            self.ts = ts;
        }
        for result in self.dom.children_of_kind(node.id, ElementKind::Result) {
            self.result(result, node, kind)?;
        }
        Ok(())
    }

    fn result(&mut self, node: &Node, call: &Node, kind: CallLine) -> Result<()> {
        let call_id = call.str_attr(attr::CALL_ID).map_or_else(|| format!("call_loa_{}", call.id), str::to_owned);
        let mut line = match residual(node, X_CODEX)? {
            Some(mut line) => {
                let payload = payload_mut(&mut line);
                payload.insert("call_id".to_owned(), Json::from(call_id));
                match (node.attr(X_PARTS).and_then(Value::as_list), payload.get_mut("output")) {
                    (Some(lengths), Some(Json::Array(parts))) => {
                        restore_parts(&mut [parts], MESSAGE_SEPARATOR, &node.text, lengths);
                    }
                    (_, None) => {
                        payload.insert("output".to_owned(), Json::from(node.text.as_str()));
                    }
                    _ => {}
                }
                line
            }
            None => line_of(
                self.ts,
                "response_item",
                json!({ "type": kind.output_type(), "call_id": call_id, "output": node.text }),
            ),
        };
        if node.bool_attr(attr::IS_ERROR) == Some(true) {
            let extension = line.entry(LOA_KEY).or_insert_with(|| Json::Object(Map::new()));
            if let Some(fields) = extension.as_object_mut() {
                fields.insert("is_error".to_owned(), Json::Bool(true));
            }
        }
        self.push(node.int_attr(X_LINE), Json::Object(line).to_string());
        Ok(())
    }

    fn compaction(&mut self, node: &Node, at: Option<i64>) -> Result<()> {
        if let Some(ts) = node.int_attr(attr::CREATED) {
            self.ts = ts;
        }
        let line = match residual(node, X_CODEX)? {
            Some(mut line) => {
                payload_mut(&mut line).insert("message".to_owned(), Json::from(node.text.as_str()));
                line
            }
            None => line_of(self.ts, "compacted", json!({ "message": node.text })),
        };
        self.push(at, Json::Object(line).to_string());
        Ok(())
    }

    fn usage(&mut self, node: &Node, at: Option<i64>) -> Result<()> {
        if let Some(line) = residual(node, X_CODEX)? {
            self.push(at, Json::Object(line).to_string());
            return Ok(());
        }
        let parent_is_turn = self.dom.get(node.parent).is_some_and(|p| p.kind == ElementKind::Turn);
        if !parent_is_turn {
            return Ok(());
        }
        let input = node.int_attr(attr::INPUT_TOKENS).unwrap_or(0);
        let output = node.int_attr(attr::OUTPUT_TOKENS).unwrap_or(0);
        let mut usage = json!({
            "input_tokens": input,
            "cached_input_tokens": node.int_attr(attr::CACHE_READ_TOKENS).unwrap_or(0),
            "output_tokens": output,
            "reasoning_output_tokens": 0,
            "total_tokens": input + output,
        });
        if let (Some(write), Some(object)) = (node.int_attr(attr::CACHE_WRITE_TOKENS), usage.as_object_mut()) {
            object.insert("cache_write_input_tokens".to_owned(), Json::from(write));
        }
        let line = line_of(
            self.ts,
            "event_msg",
            json!({
                "type": "token_count",
                "info": { "total_token_usage": usage.clone(), "last_token_usage": usage, "model_context_window": null },
                "rate_limits": null,
            }),
        );
        self.push(at, Json::Object(line).to_string());
        Ok(())
    }
}

fn take_array(object: &mut Map<String, Json>, key: &str) -> Option<Vec<Json>> {
    match object.remove(key) {
        Some(Json::Array(items)) => Some(items),
        Some(other) => {
            object.insert(key.to_owned(), other);
            None
        }
        None => None,
    }
}
