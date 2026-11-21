//! Codex rollout files (`~/.codex/sessions/**/rollout-*.jsonl`). Every line becomes one node that records
//! its line number in `x-line`; the fields the node models are taken out of the line and the rest is kept in
//! `x-codex`. Lines with no DOM shape become `<ext>` holding the raw line. Export puts the fields back,
//! orders lines by `x-line`, and writes nodes of live sessions in the shapes Codex writes.

use super::{HARNESS, canonical, str_at};
use crate::kit::residual::{self as kit_residual, join_parts, split_parts};
use loa_contracts::harness::{AdapterError, Result};
use loa_core::{
    CallStatus, Dom, DomWriter, ElementKind as E, MessageKind, MessageStatus, Node, NodeId, PatchSink, Role,
    SCHEMA_VERSION, SessionMode, TurnStatus, Value, attr,
};
use loa_util::time::{format_iso_ms, parse_iso_ms};
use serde_json::{Map, Value as Json, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};

type Attrs = Vec<(&'static str, Value)>;
type Obj = Map<String, Json>;

const X_LINE: &str = "x-line";
const X_CODEX: &str = "x-codex";
/// Byte lengths of the text parts joined into the node text.
const X_PARTS: &str = "x-codex-parts";
/// Line number and residual of the `task_complete` or `turn_aborted` line that ended a turn.
const X_LINE_END: &str = "x-line-end";
const X_CODEX_END: &str = "x-codex-end";
/// A turn opened because the file had no `task_started` line for it.
const X_IMPLICIT: &str = "x-codex-implicit";
/// Top level key on written lines, for DOM data Codex has no field for.
const LOA_KEY: &str = "x-loa";
const TEXT_PARTS: &[&str] = &["input_text", "output_text", "summary_text", "reasoning_text", "text"];
const REASONING: &str = "\n\n";
/// Starts of user-role messages Codex injects itself.
const INJECTED: &[&str] =
    &["<environment_context>", "<user_instructions>", "# AGENTS.md instructions", "<permissions instructions>", "<turn_aborted>"];

fn format_error(line: usize, detail: impl std::fmt::Display) -> AdapterError {
    AdapterError::Format(format!("line {}: {detail}", line + 1))
}

fn take_str(obj: &mut Obj, key: &str) -> Option<String> {
    match obj.get(key)? {
        Json::String(_) => obj.remove(key).and_then(|v| if let Json::String(s) = v { Some(s) } else { None }),
        _ => None,
    }
}

fn is_text_part(part: &Obj) -> bool {
    part.get("type").and_then(Json::as_str).is_some_and(|t| TEXT_PARTS.contains(&t))
}

/// Moves the text of every text part under `keys` into one string, with the part lengths.
fn take_parts(payload: &mut Obj, keys: &[&str], separator: &str) -> (String, Vec<Value>) {
    let mut texts = Vec::new();
    for key in keys {
        let parts = payload.get_mut(*key).and_then(Json::as_array_mut).into_iter().flatten();
        texts.extend(parts.filter_map(Json::as_object_mut).filter(|p| is_text_part(p)).filter_map(|p| take_str(p, "text")));
    }
    join_parts(texts.iter().map(String::as_str), separator)
}

/// Puts `text` back into the parts [`take_parts`] emptied; edited text all goes to the first part.
fn restore_parts(payload: &mut Obj, keys: &[&str], separator: &str, text: &str, lengths: &[Value]) {
    let mut arrays: Vec<(usize, &mut Json)> =
        payload.iter_mut().filter_map(|(k, v)| Some((keys.iter().position(|key| key == k)?, v))).collect();
    arrays.sort_by_key(|(index, _)| *index);
    let parts = arrays.into_iter().filter_map(|(_, v)| v.as_array_mut()).flatten().filter_map(Json::as_object_mut);
    let mut slots: Vec<&mut Obj> = parts.filter(|p| !p.contains_key("text") && is_text_part(p)).collect();
    let pieces = split_parts(text, separator, lengths).filter(|pieces| pieces.len() == slots.len());
    for (index, slot) in slots.iter_mut().enumerate() {
        let piece = pieces.as_ref().map_or(if index == 0 { text } else { "" }, |pieces| pieces[index]);
        slot.insert("text".to_owned(), piece.into());
    }
}

/// Reads a rollout into the empty `sink`; `node` is written to `<session node>`.
pub fn import(reader: impl BufRead, sink: &dyn PatchSink, node: &str) -> Result<()> {
    let mut importer = Importer {
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
    };
    for (index, line) in reader.lines().enumerate() {
        let line = line?;
        if !line.trim().is_empty() {
            importer.line(index, &line)?;
        }
    }
    if importer.session.is_none() {
        return Err(AdapterError::Format("the rollout has no session_meta line".to_owned()));
    }
    importer.close_turn(TurnStatus::Cancelled)
}

struct Turn {
    node: NodeId,
    id: Option<String>,
    implicit: bool,
    /// The turn holds agent output, so a new user prompt starts a new implicit turn.
    replied: bool,
    /// Calls without an output yet.
    open_calls: Vec<NodeId>,
}

/// A line whose payload is an object: its number, the payload, and the rest.
struct Line {
    at: i64,
    payload: Obj,
    rest: Obj,
}

impl Line {
    /// The line without the fields taken out of it, as JSON.
    fn residual(mut self) -> String {
        self.rest.insert("payload".to_owned(), Json::Object(self.payload));
        canonical(Json::Object(self.rest))
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum CallLine {
    Function,
    Custom,
    LocalShell,
}

struct Importer<'a> {
    sink: &'a dyn PatchSink,
    node: &'a str,
    ts: i64,
    session: Option<NodeId>,
    turn: Option<Turn>,
    calls: HashMap<String, NodeId>,
    turns: Vec<NodeId>,
    covered: usize,
    model: Option<String>,
    effort: Option<String>,
    mode: Option<SessionMode>,
}

impl<'a> Importer<'a> {
    fn w(&self) -> DomWriter<'a> {
        DomWriter::new(self.sink).at(self.ts)
    }

    /// Inserts the node of line `l`, recording its number and residual.
    fn put(&self, parent: NodeId, kind: E, mut attrs: Attrs, text: String, l: Line) -> Result<NodeId> {
        attrs.extend([(X_LINE, l.at.into()), (X_CODEX, l.residual().into())]);
        Ok(self.w().insert(parent, kind, attrs, text)?)
    }

    fn line(&mut self, index: usize, raw: &str) -> Result<()> {
        let Json::Object(mut rest) = serde_json::from_str(raw).map_err(|e| format_error(index, e))? else {
            return Err(format_error(index, "not a JSON object"));
        };
        if let Some(ts) = rest.get("timestamp").and_then(Json::as_str).and_then(parse_iso_ms) {
            self.ts = ts;
        }
        let kind = rest.get("type").and_then(Json::as_str).unwrap_or_default().to_owned();
        let sub = rest.get("payload").and_then(|p| str_at(p, "type")).map(str::to_owned);
        let at = i64::try_from(index).unwrap_or(i64::MAX);
        let Some(session) = self.session else {
            if kind != "session_meta" {
                return Err(format_error(index, "the rollout doesn't start with session_meta"));
            }
            let Some(Json::Object(mut payload)) = rest.remove("payload") else {
                return Err(format_error(index, "session_meta without payload"));
            };
            let created = payload.get("timestamp").and_then(Json::as_str).and_then(parse_iso_ms).unwrap_or(self.ts);
            let keys: Vec<&str> = ["id", "cwd"].into_iter().filter(|k| payload.get(*k).is_some_and(Json::is_string)).collect();
            let mut remaining = kit_residual::take(&mut payload, &keys);
            let native = take_str(&mut payload, "id").ok_or_else(|| format_error(index, "session_meta without id"))?;
            let cwd = take_str(&mut payload, "cwd");
            remaining.extend(payload);
            let payload = remaining;
            let mut attrs: Attrs = vec![
                (attr::ID, native.as_str().into()),
                (attr::HARNESS, HARNESS.into()),
                (attr::NODE, self.node.into()),
                (attr::CREATED, created.into()),
                (attr::SCHEMA, i64::from(SCHEMA_VERSION).into()),
            ];
            attrs.extend(cwd.map(|cwd| (attr::CWD, cwd.into())));
            attrs.push((attr::NATIVE_ID, native.into()));
            self.session = Some(self.put(NodeId::DOCUMENT, E::Session, attrs, String::new(), Line { at, payload, rest })?);
            return Ok(());
        };
        if kind == "turn_context" {
            self.turn_context(session, rest.get("payload"))?;
        } else if let Some(Json::Object(payload)) = rest.remove("payload") {
            let l = Line { at, payload, rest };
            let kept = match (kind.as_str(), sub.as_deref().unwrap_or_default()) {
                ("response_item", "message") => self.message(l, false)?,
                ("response_item", "reasoning") => self.message(l, true)?,
                ("response_item", "function_call") => self.call(l, CallLine::Function)?,
                ("response_item", "custom_tool_call") => self.call(l, CallLine::Custom)?,
                ("response_item", "local_shell_call") => self.call(l, CallLine::LocalShell)?,
                ("response_item", "function_call_output" | "custom_tool_call_output") => self.output(l)?,
                ("compacted", _) => self.compacted(session, l)?,
                ("event_msg", "task_started" | "turn_started") => self.task_started(session, l)?,
                ("event_msg", "task_complete" | "turn_complete") => {
                    let failed = l.rest.get(LOA_KEY).and_then(|x| str_at(x, "status")) == Some("error");
                    self.task_end(l, if failed { TurnStatus::Error } else { TurnStatus::Done })?
                }
                ("event_msg", "turn_aborted") => self.task_end(l, TurnStatus::Cancelled)?,
                ("event_msg", "token_count") => self.token_count(l)?,
                _ => false,
            };
            if kept {
                return Ok(());
            }
        }
        let parent = self.turn.as_ref().map_or(session, |t| t.node);
        let ty = sub.map_or_else(|| kind.clone(), |sub| format!("{kind}:{sub}"));
        let attrs: Attrs = vec![(attr::HARNESS, HARNESS.into()), (attr::TYPE, ty.into()), (X_LINE, at.into())];
        self.w().insert(parent, E::Ext, attrs, raw)?;
        Ok(())
    }

    /// The open turn, opening an implicit one when the file has no turn markers.
    fn turn_for(&mut self, prompt: bool, replied: bool) -> Result<NodeId> {
        if self.turn.as_ref().is_none_or(|t| prompt && t.implicit && t.replied) {
            self.close_turn(TurnStatus::Done)?;
            let session = self.session.ok_or_else(|| AdapterError::Format("no session".to_owned()))?;
            let attrs: Attrs =
                vec![(attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, self.ts.into()), (X_IMPLICIT, true.into())];
            let node = self.w().insert(session, E::Turn, attrs, "")?;
            self.turns.push(node);
            self.turn = Some(Turn { node, id: None, implicit: true, replied: false, open_calls: Vec::new() });
        }
        let turn = self.turn.as_mut().ok_or_else(|| AdapterError::Format("no turn".to_owned()))?;
        turn.replied |= replied;
        Ok(turn.node)
    }

    /// Ends the open turn: calls without output are cancelled; an implicit turn is always `done`.
    fn close_turn(&mut self, status: TurnStatus) -> Result<()> {
        let Some(turn) = self.turn.take() else { return Ok(()) };
        for call in &turn.open_calls {
            self.w().set(*call, attr::STATUS, CallStatus::Cancelled)?;
        }
        Ok(self.w().set(turn.node, attr::STATUS, if turn.implicit { TurnStatus::Done } else { status })?)
    }

    fn message(&mut self, mut l: Line, thinking: bool) -> Result<bool> {
        let injected = l.payload.get("content").and_then(Json::as_array).is_some_and(|parts| {
            let mut texts = parts.iter().filter_map(|p| str_at(p, "text")).peekable();
            texts.peek().is_some() && texts.all(|t| INJECTED.iter().any(|prefix| t.starts_with(prefix)))
        });
        let (text, lengths) = match thinking {
            true => take_parts(&mut l.payload, &["summary", "content"], REASONING),
            false => take_parts(&mut l.payload, &["content"], ""),
        };
        let role = match l.payload.get("role").and_then(Json::as_str) {
            _ if thinking => Role::Assistant,
            Some("assistant") => Role::Assistant,
            Some("user") if !injected => Role::User,
            _ => Role::System,
        };
        let id = take_str(&mut l.payload, "id");
        let parent = self.turn_for(role == Role::User, role == Role::Assistant)?;
        let mut attrs: Attrs = vec![(attr::ROLE, role.into()), (attr::STATUS, MessageStatus::Done.into()), (X_PARTS, Value::List(lengths))];
        attrs.extend(thinking.then(|| (attr::KIND, MessageKind::Thinking.into())));
        attrs.extend(id.map(|id| (attr::ID, id.into())));
        self.put(parent, E::Message, attrs, text, l).map(|_| true)
    }

    fn call(&mut self, mut l: Line, kind: CallLine) -> Result<bool> {
        let (tool, input, title) = match kind {
            CallLine::LocalShell => {
                let action = l.payload.remove("action");
                let command = action.as_ref().and_then(|a| a.get("command")).and_then(Json::as_array);
                let title = command.map(|parts| parts.iter().filter_map(Json::as_str).collect::<Vec<_>>().join(" "));
                ("local_shell".to_owned(), action.map(canonical), title)
            }
            _ => {
                let Some(tool) = take_str(&mut l.payload, "name") else { return Ok(false) };
                (tool, take_str(&mut l.payload, if kind == CallLine::Function { "arguments" } else { "input" }), None)
            }
        };
        let call_id = take_str(&mut l.payload, "call_id");
        let key = call_id.clone().or_else(|| l.payload.get("id").and_then(Json::as_str).map(str::to_owned));
        let parent = self.turn_for(false, true)?;
        let mut attrs: Attrs = vec![(attr::TOOL, tool.into()), (attr::STATUS, CallStatus::Running.into())];
        attrs.extend(call_id.map(|id| (attr::CALL_ID, id.into())));
        attrs.extend(title.map(|title| (attr::TITLE, title.into())));
        let call = self.put(parent, E::Call, attrs, String::new(), l)?;
        if let Some(input) = input {
            let format: Attrs = (kind == CallLine::Custom).then(|| (attr::FORMAT, "text".into())).into_iter().collect();
            self.w().insert(call, E::Input, format, input)?;
        }
        if let Some(key) = key {
            self.calls.insert(key, call);
        }
        self.turn.iter_mut().for_each(|t| t.open_calls.push(call));
        Ok(true)
    }

    fn output(&mut self, mut l: Line) -> Result<bool> {
        let Some(call) = str_at_obj(&l.payload, "call_id").and_then(|id| self.calls.get(id)).copied() else { return Ok(false) };
        l.payload.remove("call_id");
        let mut attrs = Attrs::new();
        let text = match l.payload.get("output") {
            Some(Json::String(_)) => take_str(&mut l.payload, "output").unwrap_or_default(),
            Some(Json::Array(_)) => {
                let (text, lengths) = take_parts(&mut l.payload, &["output"], "");
                attrs.push((X_PARTS, Value::List(lengths)));
                text
            }
            Some(other) => canonical(other.clone()),
            None => String::new(),
        };
        let is_error = l.rest.get(LOA_KEY).and_then(|x| x.get("is_error")).and_then(Json::as_bool) == Some(true);
        if is_error {
            attrs.push((attr::IS_ERROR, true.into()));
        }
        self.put(call, E::Result, attrs, text, l)?;
        self.w().close(call, if is_error { CallStatus::Error } else { CallStatus::Done })?;
        self.turn.iter_mut().for_each(|t| t.open_calls.retain(|c| *c != call));
        Ok(true)
    }

    fn compacted(&mut self, session: NodeId, mut l: Line) -> Result<bool> {
        let summary = take_str(&mut l.payload, "message").unwrap_or_default();
        let covers = self.turns[self.covered..].iter().map(|t| Value::from(*t)).collect();
        self.covered = self.turns.len();
        let attrs: Attrs = vec![(attr::COVERS, Value::List(covers)), (attr::CREATED, self.ts.into())];
        self.put(session, E::Compaction, attrs, summary, l).map(|_| true)
    }

    fn turn_context(&mut self, session: NodeId, payload: Option<&Json>) -> Result<()> {
        let Some(payload) = payload else { return Ok(()) };
        let w = self.w();
        for (field, name, key) in [(&mut self.model, attr::MODEL, "model"), (&mut self.effort, attr::EFFORT, "effort")] {
            if let Some(value) = str_at(payload, key)
                && field.as_deref() != Some(value)
            {
                *field = Some(value.to_owned());
                w.set(session, name, value)?;
            }
        }
        if let Some(mode) = payload.get("collaboration_mode").and_then(|m| str_at(m, "mode")) {
            let mode = if mode == "plan" { SessionMode::Plan } else { SessionMode::Normal };
            if self.mode.replace(mode) != Some(mode) {
                w.set(session, attr::MODE, mode)?;
            }
        }
        Ok(())
    }

    fn task_started(&mut self, session: NodeId, mut l: Line) -> Result<bool> {
        let id = take_str(&mut l.payload, "turn_id");
        self.close_turn(TurnStatus::Cancelled)?;
        let mut attrs: Attrs = vec![(attr::STATUS, TurnStatus::Running.into()), (attr::STARTED, self.ts.into())];
        attrs.extend(id.as_deref().map(|id| (attr::ID, id.into())));
        let node = self.put(session, E::Turn, attrs, String::new(), l)?;
        self.turns.push(node);
        self.turn = Some(Turn { node, id, implicit: false, replied: false, open_calls: Vec::new() });
        Ok(true)
    }

    fn task_end(&mut self, mut l: Line, status: TurnStatus) -> Result<bool> {
        let Some(turn) = &self.turn else { return Ok(false) };
        let (node, id) = (turn.node, turn.id.clone());
        match (id.as_deref(), str_at_obj(&l.payload, "turn_id")) {
            (Some(expected), Some(actual)) if expected != actual => return Ok(false),
            (Some(_), Some(_)) => drop(l.payload.remove("turn_id")),
            _ => {}
        }
        let (at, w) = (l.at, self.w());
        w.set(node, X_CODEX_END, l.residual())?;
        w.set(node, X_LINE_END, at)?;
        w.set(node, attr::ENDED, self.ts)?;
        self.close_turn(status).map(|()| true)
    }

    fn token_count(&mut self, l: Line) -> Result<bool> {
        let (Some(usage), Some(turn)) = (l.payload.get("info").and_then(|i| i.get("last_token_usage")), &self.turn) else {
            return Ok(false);
        };
        let names = [
            ("input_tokens", attr::INPUT_TOKENS),
            ("output_tokens", attr::OUTPUT_TOKENS),
            ("cached_input_tokens", attr::CACHE_READ_TOKENS),
            ("cache_write_input_tokens", attr::CACHE_WRITE_TOKENS),
        ];
        let attrs = names.iter().filter_map(|(key, name)| Some((*name, usage.get(*key)?.as_i64()?.into()))).collect();
        self.put(turn.node, E::Usage, attrs, String::new(), l).map(|_| true)
    }
}

fn str_at_obj<'a>(obj: &'a Obj, key: &str) -> Option<&'a str> {
    obj.get(key)?.as_str()
}

/// Writes `dom` as a rollout.
pub fn export(dom: &Dom, out: &mut impl Write) -> Result<()> {
    let root = dom.root().ok_or_else(|| AdapterError::Format("the DOM is empty".to_owned()))?;
    if root.kind != E::Session {
        return Err(AdapterError::Format("the DOM root isn't a <session>".to_owned()));
    }
    let mut exporter = Exporter { dom, lines: Vec::new(), key: -1, ts: root.int_attr(attr::CREATED).unwrap_or(0) };
    exporter.node(root)?;
    exporter.lines.sort_by_key(|(key, _)| *key);
    for (_, line) in &exporter.lines {
        writeln!(out, "{line}")?;
    }
    Ok(())
}

struct Exporter<'d> {
    dom: &'d Dom,
    lines: Vec<(i64, String)>,
    /// Highest line number so far; lines of live nodes sort right after it.
    key: i64,
    /// Timestamp of lines written for live nodes.
    ts: i64,
}

/// The residual JSON in attribute `name`; an error when malformed.
fn residual(node: &Node, name: &str) -> Result<Option<Obj>> {
    match node.str_attr(name).map(serde_json::from_str::<Json>) {
        None => Ok(None),
        Some(Ok(Json::Object(_))) if name == kit_residual::attr_name(HARNESS) => Ok(Some(kit_residual::restore(node, HARNESS))),
        Some(Ok(Json::Object(obj))) => Ok(Some(obj)),
        Some(_) => Err(AdapterError::Format(format!("node {} has a malformed {name}", node.id))),
    }
}

fn payload_mut(line: &mut Obj) -> &mut Obj {
    let payload = line.entry("payload").or_insert_with(|| json!({}));
    if !payload.is_object() {
        *payload = json!({});
    }
    payload.as_object_mut().expect("the payload was made an object")
}

fn insert(obj: &mut Obj, key: &str, value: impl Into<Json>) {
    obj.insert(key.to_owned(), value.into());
}

fn call_id(call: &Node) -> String {
    call.str_attr(attr::CALL_ID).map_or_else(|| format!("call_loa_{}", call.id), str::to_owned)
}

impl Exporter<'_> {
    fn line_of(&self, kind: &str, payload: Json) -> Obj {
        let mut line = Obj::new();
        insert(&mut line, "timestamp", format_iso_ms(self.ts));
        insert(&mut line, "type", kind);
        insert(&mut line, "payload", payload);
        line
    }

    fn push(&mut self, at: Option<i64>, line: String) {
        self.key = self.key.max(at.unwrap_or(self.key));
        self.lines.push((at.unwrap_or(self.key), line));
    }

    fn push_line(&mut self, at: Option<i64>, line: Obj) {
        self.push(at, canonical(Json::Object(line)));
    }

    fn stamp(&mut self, node: &Node, name: &str) {
        if let Some(ts) = node.int_attr(name) {
            self.ts = ts;
        }
    }

    fn node(&mut self, node: &Node) -> Result<()> {
        let at = node.int_attr(X_LINE);
        match node.kind {
            E::Session => {
                self.session(node, at)?;
                self.dom.children(node.id).try_for_each(|child| self.node(child))
            }
            E::Turn => self.turn(node, at),
            E::Message => self.message(node, at),
            E::Call => self.call(node, at),
            E::Compaction => {
                self.stamp(node, attr::CREATED);
                let line = match residual(node, X_CODEX)? {
                    Some(mut line) => {
                        insert(payload_mut(&mut line), "message", node.text.as_str());
                        line
                    }
                    None => self.line_of("compacted", json!({ "message": node.text })),
                };
                self.push_line(at, line);
                Ok(())
            }
            E::Usage => self.usage(node, at),
            E::Ext if at.is_some() && node.str_attr(attr::HARNESS) == Some(HARNESS) => {
                self.push(at, node.text.clone());
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
                insert(payload, "id", id);
                if let Some(cwd) = node.str_attr(attr::CWD) {
                    insert(payload, "cwd", cwd);
                }
                line
            }
            None => self.line_of(
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
        self.push_line(at, line);
        Ok(())
    }

    fn turn(&mut self, node: &Node, at: Option<i64>) -> Result<()> {
        self.stamp(node, attr::STARTED);
        let id = node.str_attr(attr::ID);
        let turn_id = id.map_or_else(|| format!("loa-turn-{}", node.id), str::to_owned);
        let implicit = node.bool_attr(X_IMPLICIT) == Some(true);
        if !implicit {
            let line = match residual(node, X_CODEX)? {
                Some(mut line) => {
                    if let Some(id) = id {
                        insert(payload_mut(&mut line), "turn_id", id);
                    }
                    line
                }
                None => self.line_of("event_msg", json!({ "type": "task_started", "turn_id": turn_id, "model_context_window": null })),
            };
            self.push_line(at, line);
        }
        self.dom.children(node.id).try_for_each(|child| self.node(child))?;
        self.stamp(node, attr::ENDED);
        let status = node.str_attr(attr::STATUS).and_then(TurnStatus::parse);
        let end = match residual(node, X_CODEX_END)? {
            Some(mut line) => {
                let payload = payload_mut(&mut line);
                if let (false, Some(id)) = (payload.contains_key("turn_id"), id) {
                    insert(payload, "turn_id", id);
                }
                Some(line)
            }
            None if implicit => None,
            None => match status {
                Some(TurnStatus::Done | TurnStatus::Error) => Some(self.line_of(
                    "event_msg",
                    json!({ "type": "task_complete", "turn_id": turn_id, "last_agent_message": null }),
                )),
                Some(TurnStatus::Cancelled) => {
                    Some(self.line_of("event_msg", json!({ "type": "turn_aborted", "turn_id": turn_id, "reason": "interrupted" })))
                }
                _ => None,
            },
        };
        if let Some(mut line) = end {
            if status == Some(TurnStatus::Error) {
                insert(&mut line, LOA_KEY, json!({ "status": "error" }));
            }
            self.push_line(node.int_attr(X_LINE_END), line);
        }
        Ok(())
    }

    fn message(&mut self, node: &Node, at: Option<i64>) -> Result<()> {
        let thinking = node.str_attr(attr::KIND) == Some(MessageKind::Thinking.as_str());
        let id = node.str_attr(attr::ID);
        let mut line = match residual(node, X_CODEX)? {
            Some(mut line) => {
                let payload = payload_mut(&mut line);
                let lengths = node.attr(X_PARTS).and_then(Value::as_list).unwrap_or_default();
                let separator = if thinking { REASONING } else { "" };
                restore_parts(payload, &["summary", "content"], separator, &node.text, lengths);
                line
            }
            None if thinking => {
                let summary = if node.text.is_empty() { json!([]) } else { json!([{ "type": "summary_text", "text": node.text }]) };
                let payload = json!({ "type": "reasoning", "summary": summary, "content": null, "encrypted_content": null });
                self.line_of("response_item", payload)
            }
            None => {
                let (role, part) = match node.str_attr(attr::ROLE).and_then(Role::parse) {
                    Some(Role::Assistant) => ("assistant", "output_text"),
                    Some(Role::User) => ("user", "input_text"),
                    _ => ("developer", "input_text"),
                };
                self.line_of("response_item", json!({ "type": "message", "role": role, "content": [{ "type": part, "text": node.text }] }))
            }
        };
        if let Some(id) = id {
            insert(payload_mut(&mut line), "id", id);
        }
        self.push_line(at, line);
        Ok(())
    }

    fn call(&mut self, node: &Node, at: Option<i64>) -> Result<()> {
        self.stamp(node, attr::STARTED);
        let tool = node.str_attr(attr::TOOL).unwrap_or_default();
        let input = self.dom.children_of_kind(node.id, E::Input).next();
        let imported = residual(node, X_CODEX)?;
        let kind = match &imported {
            Some(line) => match line.get("payload").and_then(|p| str_at(p, "type")) {
                Some("custom_tool_call") => CallLine::Custom,
                Some("local_shell_call") => CallLine::LocalShell,
                _ => CallLine::Function,
            },
            None if input.and_then(|i| i.str_attr(attr::FORMAT)) == Some("text") => CallLine::Custom,
            None if tool == "local_shell" && input.is_some_and(|i| serde_json::from_str::<Obj>(&i.text).is_ok()) => CallLine::LocalShell,
            None => CallLine::Function,
        };
        let is_live = imported.is_none();
        let ty = match kind {
            CallLine::Function => "function_call",
            CallLine::Custom => "custom_tool_call",
            CallLine::LocalShell => "local_shell_call",
        };
        let mut line = imported.unwrap_or_else(|| self.line_of("response_item", json!({ "type": ty })));
        let payload = payload_mut(&mut line);
        if is_live || node.attr(attr::CALL_ID).is_some() {
            insert(payload, "call_id", call_id(node));
        }
        let input = input.map(|i| i.text.as_str());
        match (kind, input) {
            (CallLine::LocalShell, Some(input)) => {
                insert(payload, "action", serde_json::from_str(input).unwrap_or_else(|_| Json::from(input)))
            }
            (CallLine::LocalShell, None) => {}
            (_, input) => {
                insert(payload, "name", tool);
                let field = if kind == CallLine::Function { "arguments" } else { "input" };
                match input {
                    Some(input) => insert(payload, field, input),
                    None if is_live && kind == CallLine::Function => insert(payload, field, "{}"),
                    None => {}
                }
            }
        }
        if is_live && kind != CallLine::Function {
            insert(payload, "status", "completed");
        }
        self.push_line(at, line);
        self.stamp(node, attr::ENDED);
        for result in self.dom.children_of_kind(node.id, E::Result) {
            self.result(result, node, kind)?;
        }
        Ok(())
    }

    fn result(&mut self, node: &Node, call: &Node, kind: CallLine) -> Result<()> {
        let mut line = match residual(node, X_CODEX)? {
            Some(mut line) => {
                let payload = payload_mut(&mut line);
                insert(payload, "call_id", call_id(call));
                match (node.attr(X_PARTS).and_then(Value::as_list), payload.get("output")) {
                    (Some(lengths), Some(Json::Array(_))) => restore_parts(payload, &["output"], "", &node.text, lengths),
                    (_, None) => insert(payload, "output", node.text.as_str()),
                    _ => {}
                }
                line
            }
            None => {
                let ty = if kind == CallLine::Custom { "custom_tool_call_output" } else { "function_call_output" };
                self.line_of("response_item", json!({ "type": ty, "call_id": call_id(call), "output": node.text }))
            }
        };
        if node.bool_attr(attr::IS_ERROR) == Some(true) {
            let extension = line.entry(LOA_KEY).or_insert_with(|| json!({}));
            if let Some(fields) = extension.as_object_mut() {
                insert(fields, "is_error", true);
            }
        }
        self.push_line(node.int_attr(X_LINE), line);
        Ok(())
    }

    fn usage(&mut self, node: &Node, at: Option<i64>) -> Result<()> {
        if let Some(line) = residual(node, X_CODEX)? {
            self.push_line(at, line);
            return Ok(());
        }
        if !self.dom.get(node.parent).is_some_and(|p| p.kind == E::Turn) {
            return Ok(());
        }
        let count = |name| node.int_attr(name).unwrap_or(0);
        let (input, output) = (count(attr::INPUT_TOKENS), count(attr::OUTPUT_TOKENS));
        let mut usage = json!({
            "input_tokens": input,
            "cached_input_tokens": count(attr::CACHE_READ_TOKENS),
            "output_tokens": output,
            "reasoning_output_tokens": 0,
            "total_tokens": input + output,
        });
        if let (Some(write), Some(obj)) = (node.int_attr(attr::CACHE_WRITE_TOKENS), usage.as_object_mut()) {
            insert(obj, "cache_write_input_tokens", write);
        }
        let info = json!({ "total_token_usage": usage.clone(), "last_token_usage": usage, "model_context_window": null });
        let line = self.line_of("event_msg", json!({ "type": "token_count", "info": info, "rate_limits": null }));
        self.push_line(at, line);
        Ok(())
    }
}
