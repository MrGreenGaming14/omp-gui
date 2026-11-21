//! Claude Code transcripts (`~/.claude/projects/<slug>/<session>.jsonl` plus
//! the `<session>/` side directory) to and from the DOM. Import keeps what the
//! DOM doesn't model, so export rewrites the file line for line:
//! - `x-claude`: the line without `message.content`, on its first element
//! - `x-claude-line`, `x-claude-part`: the line and content block index
//! - `x-claude-block`: the block without the fields the element holds
//! - `x-claude-content`: how a result or compaction text was encoded (`string`,
//!   `texts` joined by lines with `x-claude-split` lengths, `json`, `absent`)
//! - `x-claude-file`: a subagent `<job>`'s transcript; `x-path`: a side file
//!
//! Uncovered lines are `<ext>` holding the line verbatim. Elements without
//! these attributes (written by the app) export as fresh lines.

use crate::claude::{HARNESS, USAGE};
use crate::kit::{NativeCodec, NativeSession, native, residual};
use loa_contracts::harness::{AdapterError, Result};
use loa_core::{
    CallStatus, Dom, DomWriter, ElementKind as E, JobKind, JobStatus, MessageKind, MessageStatus, Node, NodeId,
    PatchSink, Role, SCHEMA_VERSION, TurnStatus, Value as V, attr,
};
use loa_util::time;
use serde_json::{Map, Value as J, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

const X_ENVELOPE: &str = "x-claude";
const X_BLOCK: &str = "x-claude-block";
const X_LINE: &str = "x-claude-line";
const X_PART: &str = "x-claude-part";
const X_CONTENT: &str = "x-claude-content";
const X_SPLIT: &str = "x-claude-split";
const X_FILE: &str = "x-claude-file";
const X_PATH: &str = "x-path";
const TEXTS: &str = "texts";
const JSON: &str = "json";

type Attrs = Vec<(&'static str, V)>;

/// The side directory of a transcript: its path without extension.
fn side_dir(transcript: &Path) -> Option<PathBuf> {
    transcript.extension().map(|_| transcript.with_extension(""))
}

fn str_field(value: &J, name: &str) -> Option<String> {
    value.get(name)?.as_str().map(str::to_owned)
}

/// `value` as JSON text with the keys of every object sorted.
fn sorted_text(mut value: J) -> String {
    value.sort_all_objects();
    value.to_string()
}

/// `block` without `fields`, as JSON text.
fn leftover(block: &J, fields: &[&str]) -> String {
    sorted_text(match block {
        J::Object(object) => J::Object(residual::take(&mut object.clone(), fields)),
        other => other.clone(),
    })
}

/// Result or compaction content as element text and the attributes saying how
/// it was encoded (`format` = json too when `format`); `texts` content also
/// gives its blocks without their text.
fn encode(content: Option<&J>, format: bool) -> (String, Attrs, Option<Vec<J>>) {
    let is_text = |b: &J| b["type"] == "text" && b["text"].is_string();
    let (text, kind) = match content {
        None => (String::new(), "absent"),
        Some(J::String(text)) => (text.clone(), "string"),
        Some(J::Array(blocks)) if !blocks.is_empty() && blocks.iter().all(is_text) => {
            let (text, split) = residual::join_parts(blocks.iter().filter_map(|b| b["text"].as_str()), "\n");
            let items = blocks.iter().map(|b| J::Object(residual::take(&mut b.as_object().cloned().unwrap_or_default(), &["text"])));
            return (text, vec![(X_CONTENT, V::from(TEXTS)), (X_SPLIT, V::List(split))], Some(items.collect()));
        }
        Some(other) => (other.to_string(), JSON),
    };
    let mut attrs: Attrs = vec![(X_CONTENT, V::from(kind))];
    attrs.extend((format && kind == JSON).then(|| (attr::FORMAT, V::from(JSON))));
    (text, attrs, None)
}

/// Claude Code's [`NativeCodec`]: `~/.claude/projects/<project>/<uuid>.jsonl`.
pub struct ClaudeNative;

impl NativeCodec for ClaudeNative {
    fn import(path: &Path, sink: &dyn PatchSink) -> Result<()> {
        import(path, sink)
    }

    fn export(dom: &Dom, out: &Path) -> Result<()> {
        export(dom, out)
    }

    fn discover(home: &Path) -> Vec<NativeSession> {
        let files = native::walk(&home.join(".claude/projects"), 1, &|name| name.ends_with(".jsonl"));
        files.into_iter().filter_map(|path| NativeSession::at(HARNESS, path.clone(), native::parent_name(&path), None)).collect()
    }
}

/// Imports one transcript into `sink` (empty DOM), side files included. The
/// root gets `id` = the session id and `node` = "", which the app re-sets; a
/// subagent `<job>` carries `x-claude-file` for the app to import as a child session.
pub fn import(path: &Path, sink: &dyn PatchSink) -> Result<()> {
    let source = std::fs::read_to_string(path)?;
    let parse = |text: &str| {
        let mut value = serde_json::from_str::<J>(text).ok()?;
        value.sort_all_objects();
        Some(value)
    };
    let lines: Vec<(&str, Option<J>)> = source.split('\n').filter(|t| !t.trim().is_empty()).map(|t| (t, parse(t))).collect();
    let mut files = Vec::new();
    if let Some(dir) = side_dir(path).filter(|d| d.is_dir()) {
        let mut pending = vec![(dir, String::new())];
        while let Some((dir, prefix)) = pending.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    pending.push((entry.path(), rel));
                } else if kind.is_file() {
                    files.push((rel, std::fs::read(entry.path())?));
                }
            }
        }
        files.sort();
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut importer = Importer::new(sink, &lines, &stem, &files)?;
    for (index, (text, json)) in lines.iter().enumerate() {
        // A file never holds 2^63 lines.
        importer.line(index as i64, text, json.as_ref())?;
    }
    importer.finish(files)
}

/// What `subagents/agent-<id>.meta.json` says about one subagent.
struct Subagent {
    agent_id: String,
    agent_type: Option<String>,
    description: Option<String>,
}

/// The line the elements being made come from.
struct Origin {
    index: i64,
    /// The line without `message.content`, as JSON text.
    envelope: String,
}

impl Origin {
    fn new(index: i64, line: &Map<String, J>) -> Self {
        let mut envelope = line.clone();
        if let Some(J::Object(message)) = envelope.get_mut("message") {
            message.remove("content");
        }
        Self { index, envelope: sorted_text(J::Object(envelope)) }
    }

    /// Ties an element to block `part` (`None`: the content isn't an array); the first carries the envelope.
    fn attrs(&self, part: Option<usize>) -> Attrs {
        let mut attrs = vec![(X_LINE, V::Int(self.index))];
        attrs.extend(part.map(|p| (X_PART, V::Int(p as i64))));
        attrs.extend((part.unwrap_or(0) == 0).then(|| (X_ENVELOPE, V::from(self.envelope.as_str()))));
        attrs
    }
}

struct CallState {
    node: NodeId,
    started: i64,
    answered: bool,
    job: Option<NodeId>,
    input: Option<J>,
}

struct Importer<'a> {
    sink: &'a dyn PatchSink,
    root: NodeId,
    /// Every conversation line is a sidechain: the file is a subagent's.
    subagent_file: bool,
    /// Subagents by the tool use id that started them.
    subagents: HashMap<String, Subagent>,
    side_paths: HashSet<String>,
    /// Timestamp of the current line, or the last one seen.
    ts: i64,
    /// The open turn and the time of its last line.
    turn: Option<(NodeId, i64)>,
    uncompacted: Vec<V>,
    calls: Vec<CallState>,
    call_index: HashMap<String, usize>,
    /// Assistant `message.id`s seen, so usage lands on the first line only.
    messages: HashSet<String>,
    model: Option<String>,
    custom_title: Option<String>,
    summary: Option<String>,
    pre_tokens: Option<i64>,
}

impl<'a> Importer<'a> {
    fn new(sink: &'a dyn PatchSink, lines: &[(&str, Option<J>)], stem: &str, files: &[(String, Vec<u8>)]) -> Result<Self> {
        let objects = || lines.iter().filter_map(|(_, j)| j.as_ref());
        let first_str = |name: &str| objects().find_map(|j| str_field(j, name));
        let session_id = first_str("sessionId");
        let created = objects().find_map(|j| time::parse_iso_ms(j.get("timestamp")?.as_str()?)).unwrap_or(0);
        let mut conversation = objects().filter(|j| j["type"] == "user" || j["type"] == "assistant").peekable();
        let subagent_file = conversation.peek().is_some() && conversation.all(|j| j["isSidechain"] == true);
        let mut attrs: Attrs = vec![
            (attr::ID, V::from(session_id.clone().unwrap_or_else(|| stem.to_owned()))),
            (attr::HARNESS, V::from(HARNESS)),
            (attr::NODE, V::from("")),
            (attr::CREATED, V::Int(created)),
            (attr::SCHEMA, V::Int(i64::from(SCHEMA_VERSION))),
        ];
        attrs.extend(session_id.map(|id| (attr::NATIVE_ID, V::from(id))));
        attrs.extend(first_str("cwd").map(|cwd| (attr::CWD, V::from(cwd))));
        let root = DomWriter::new(sink).at(created).insert(NodeId::DOCUMENT, E::Session, attrs, "")?;
        let mut subagents = HashMap::new();
        for (path, bytes) in files {
            let Some(agent_id) = path.strip_prefix("subagents/agent-").and_then(|rest| rest.strip_suffix(".meta.json")) else { continue };
            let Ok(meta) = serde_json::from_slice::<J>(bytes) else { continue };
            let Some(tool_use_id) = str_field(&meta, "toolUseId") else { continue };
            let (agent_type, description) = (str_field(&meta, "agentType"), str_field(&meta, "description"));
            subagents.insert(tool_use_id, Subagent { agent_id: agent_id.to_owned(), agent_type, description });
        }
        let side_paths = files.iter().map(|(p, _)| p.clone()).collect();
        Ok(Self {
            sink,
            root,
            subagent_file,
            subagents,
            side_paths,
            ts: 0,
            turn: None,
            uncompacted: Vec::new(),
            calls: Vec::new(),
            call_index: HashMap::new(),
            messages: HashSet::new(),
            model: None,
            custom_title: None,
            summary: None,
            pre_tokens: None,
        })
    }

    fn w(&self) -> DomWriter<'a> {
        DomWriter::new(self.sink).at(self.ts)
    }

    fn line(&mut self, index: i64, text: &str, json: Option<&J>) -> Result<()> {
        if let Some(ts) = json.and_then(|j| time::parse_iso_ms(j.get("timestamp")?.as_str()?)) {
            self.ts = ts;
        }
        let Some(J::Object(object)) = json else { return self.line_ext(index, text, None) };
        let ty = object.get("type").and_then(J::as_str);
        let own_line = self.subagent_file || object.get("isSidechain") != Some(&J::Bool(true));
        match ty {
            Some("user") if own_line => self.user(index, text, object)?,
            Some("assistant") if own_line => self.assistant(index, text, object)?,
            _ => {
                match ty {
                    Some("custom-title") => self.custom_title = object.get("customTitle").and_then(J::as_str).map(str::to_owned).or(self.custom_title.take()),
                    Some("summary") => self.summary = object.get("summary").and_then(J::as_str).map(str::to_owned).or(self.summary.take()),
                    Some("system") if object["subtype"] == "compact_boundary" => {
                        self.pre_tokens = object.get("compactMetadata").and_then(|m| m.get("preTokens")?.as_i64());
                    }
                    _ => {}
                }
                self.line_ext(index, text, ty)?;
            }
        }
        if let Some((_, last_ts)) = &mut self.turn {
            *last_ts = self.ts;
        }
        Ok(())
    }

    /// The current turn, or the session before the first turn.
    fn ext_parent(&self) -> NodeId {
        self.turn.map_or(self.root, |(turn, _)| turn)
    }

    /// Keeps a whole line verbatim.
    fn line_ext(&self, index: i64, text: &str, ty: Option<&str>) -> Result<()> {
        let mut attrs: Attrs = vec![(attr::HARNESS, V::from(HARNESS)), (X_LINE, V::Int(index))];
        attrs.extend(ty.map(|ty| (attr::TYPE, V::from(ty))));
        self.w().insert(self.ext_parent(), E::Ext, attrs, text)?;
        Ok(())
    }

    /// Keeps one content block verbatim.
    fn block_ext(&self, parent: NodeId, origin: &Origin, part: usize, block: &J) -> Result<NodeId> {
        let mut attrs: Attrs = vec![(attr::HARNESS, V::from(HARNESS)), (attr::TYPE, V::from("block"))];
        attrs.extend(origin.attrs(Some(part)));
        Ok(self.w().insert(parent, E::Ext, attrs, block.to_string())?)
    }

    /// A text block as a `<message>` of `role`.
    fn text_message(&self, turn: NodeId, role: Role, uuid: Option<&str>, origin: &Origin, part: Option<usize>, block_text: (&J, &str)) -> Result<NodeId> {
        let (block, text) = block_text;
        let mut attrs = message_attrs(role, uuid);
        if part.is_some() {
            attrs.push((X_BLOCK, V::from(leftover(block, &["text"]))));
        }
        attrs.extend(origin.attrs(part));
        Ok(self.w().insert(turn, E::Message, attrs, text)?)
    }

    fn open_turn(&mut self, id: Option<&str>) -> Result<NodeId> {
        self.close_turn()?;
        let mut attrs: Attrs = vec![(attr::STATUS, V::from(TurnStatus::Done)), (attr::STARTED, V::Int(self.ts))];
        attrs.extend(id.map(|id| (attr::ID, V::from(id))));
        let node = self.w().insert(self.root, E::Turn, attrs, "")?;
        self.uncompacted.push(V::from(node));
        self.turn = Some((node, self.ts));
        Ok(node)
    }

    fn close_turn(&mut self) -> Result<()> {
        if let Some((turn, last_ts)) = self.turn.take() {
            self.w().set(turn, attr::ENDED, last_ts)?;
        }
        Ok(())
    }

    fn current_turn(&mut self) -> Result<NodeId> {
        match self.turn {
            Some((turn, _)) => Ok(turn),
            None => self.open_turn(None),
        }
    }

    fn user(&mut self, index: i64, text: &str, object: &Map<String, J>) -> Result<()> {
        let content = object.get("message").and_then(|m| m.get("content"));
        let blocks = match content {
            Some(J::String(_)) => None,
            Some(J::Array(blocks)) if !blocks.is_empty() => Some(blocks),
            _ => return self.line_ext(index, text, Some("user")),
        };
        let uuid = object.get("uuid").and_then(J::as_str);
        let origin = Origin::new(index, object);
        if object.get("isCompactSummary") == Some(&J::Bool(true)) {
            return self.compaction(&origin, content);
        }
        if let Some(blocks) = blocks.filter(|b| b.iter().any(|b| b["type"] == "tool_result")) {
            return self.results(&origin, uuid, object, blocks);
        }
        let (role, turn) = match object.get("isMeta") == Some(&J::Bool(true)) {
            true => (Role::System, self.current_turn()?),
            false => (Role::User, self.open_turn(uuid)?),
        };
        match (content, blocks) {
            (Some(J::String(text)), _) => {
                self.text_message(turn, role, uuid, &origin, None, (&J::Null, text))?;
            }
            (_, Some(blocks)) => {
                for (part, block) in blocks.iter().enumerate() {
                    match block_text(block, "text") {
                        Some(text) => self.text_message(turn, role, uuid, &origin, Some(part), (block, text))?,
                        None => self.block_ext(turn, &origin, part, block)?,
                    };
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn results(&mut self, origin: &Origin, uuid: Option<&str>, object: &Map<String, J>, blocks: &[J]) -> Result<()> {
        let single = blocks.iter().filter(|b| b["type"] == "tool_result").count() == 1;
        let agent = object.get("toolUseResult").filter(|_| single);
        for (part, block) in blocks.iter().enumerate() {
            let call = (block["type"] == "tool_result").then(|| block["tool_use_id"].as_str()).flatten();
            if let Some(&call) = call.and_then(|id| self.call_index.get(id)) {
                self.result(origin, part, block, call, agent)?;
            } else if let Some(text) = block_text(block, "text") {
                let turn = self.current_turn()?;
                self.text_message(turn, Role::User, uuid, origin, Some(part), (block, text))?;
            } else {
                self.block_ext(self.ext_parent(), origin, part, block)?;
            }
        }
        Ok(())
    }

    fn result(&mut self, origin: &Origin, part: usize, block: &J, call: usize, agent: Option<&J>) -> Result<()> {
        let (text, mut attrs, items) = encode(block.get("content"), true);
        let is_error = block.get("is_error").and_then(J::as_bool);
        let fields: &[&str] = if is_error.is_some() { &["tool_use_id", "content", "is_error"] } else { &["tool_use_id", "content"] };
        let mut rest = residual::take(&mut block.as_object().cloned().unwrap_or_default(), fields);
        rest.extend(items.map(|items| ("content".to_owned(), J::Array(items))));
        attrs.extend(is_error.map(|e| (attr::IS_ERROR, V::Bool(e))));
        attrs.push((X_BLOCK, V::from(sorted_text(J::Object(rest)))));
        attrs.extend(origin.attrs(Some(part)));
        let state = &self.calls[call];
        let (node, w) = (state.node, self.w());
        w.insert(node, E::Result, attrs, text)?;
        let failed = is_error == Some(true);
        w.close(node, if failed { CallStatus::Error } else { CallStatus::Done })?;
        let job_status = if failed { JobStatus::Error } else { JobStatus::Done };
        let job = match (state.job, agent.and_then(|a| a["agentId"].as_str())) {
            (Some(job), _) => {
                w.close(job, job_status)?;
                Some(job)
            }
            (None, Some(agent_id)) => {
                let agent_type = agent.and_then(|a| str_field(a, "agentType"));
                let mut attrs = self.job_attrs(agent_id, node, state.input.as_ref(), agent_type, None);
                attrs.extend([(attr::STATUS, V::from(job_status)), (attr::STARTED, V::Int(state.started)), (attr::ENDED, V::Int(self.ts))]);
                Some(w.insert(node, E::Job, attrs, "")?)
            }
            (None, None) => None,
        };
        self.calls[call].answered = true;
        self.calls[call].job = job;
        Ok(())
    }

    fn assistant(&mut self, index: i64, text: &str, object: &Map<String, J>) -> Result<()> {
        let message = object.get("message").and_then(J::as_object);
        let Some(blocks) = message.and_then(|m| m.get("content")?.as_array()).filter(|b| !b.is_empty()) else {
            return self.line_ext(index, text, Some("assistant"));
        };
        let turn = self.current_turn()?;
        let model = message.and_then(|m| m.get("model")?.as_str());
        if let Some(model) = model {
            self.model = Some(model.to_owned());
        }
        let uuid = object.get("uuid").and_then(J::as_str);
        let origin = Origin::new(index, object);
        let first_line = message.and_then(|m| m.get("id")?.as_str()).is_none_or(|id| self.messages.insert(id.to_owned()));
        let mut usage_parent = turn;
        for (part, block) in blocks.iter().enumerate() {
            let text = match block["type"].as_str() {
                Some(field @ ("text" | "thinking")) => block_text(block, field).map(|t| (t, field)),
                _ => None,
            };
            let call = (block["type"] == "tool_use").then(|| Some((block["id"].as_str()?, block["name"].as_str()?))).flatten();
            let node = if let Some((text, field)) = text {
                let mut attrs = message_attrs(Role::Assistant, uuid);
                attrs.extend((field == "thinking").then(|| (attr::KIND, V::from(MessageKind::Thinking))));
                attrs.extend(model.map(|m| (attr::MODEL, V::from(m))));
                attrs.push((X_BLOCK, V::from(leftover(block, &[field]))));
                attrs.extend(origin.attrs(Some(part)));
                self.w().insert(turn, E::Message, attrs, text)?
            } else if let Some((id, name)) = call {
                self.call(turn, &origin, part, block, id, name)?
            } else {
                self.block_ext(turn, &origin, part, block)?;
                continue;
            };
            if part == 0 {
                usage_parent = node;
            }
        }
        if first_line && let Some(usage) = message.and_then(|m| m.get("usage")?.as_object()) {
            let mut attrs: Attrs = USAGE.into_iter().filter_map(|(name, key)| Some((name, V::Int(usage.get(key)?.as_i64()?)))).collect();
            attrs.extend(model.map(|m| (attr::MODEL, V::from(m))));
            self.w().insert(usage_parent, E::Usage, attrs, "")?;
        }
        Ok(())
    }

    fn call(&mut self, turn: NodeId, origin: &Origin, part: usize, block: &J, id: &str, name: &str) -> Result<NodeId> {
        let input = block.get("input");
        let mut attrs: Attrs = vec![
            (attr::TOOL, V::from(name)),
            (attr::CALL_ID, V::from(id)),
            (attr::STATUS, V::from(CallStatus::Running)),
            (attr::STARTED, V::Int(self.ts)),
        ];
        attrs.extend(call_title(input).map(|t| (attr::TITLE, V::from(t))));
        attrs.push((X_BLOCK, V::from(leftover(block, &["id", "name", "input"]))));
        attrs.extend(origin.attrs(Some(part)));
        let w = self.w();
        let node = w.insert(turn, E::Call, attrs, "")?;
        if let Some(input) = input {
            w.insert(node, E::Input, [(attr::FORMAT, "json")], input.to_string())?;
        }
        let mut job = None;
        if let Some(subagent) = self.subagents.get(id) {
            let mut attrs = self.job_attrs(&subagent.agent_id, node, input, subagent.agent_type.clone(), subagent.description.clone());
            attrs.extend([(attr::STATUS, V::from(JobStatus::Running)), (attr::STARTED, V::Int(self.ts))]);
            job = Some(w.insert(node, E::Job, attrs, "")?);
        }
        self.call_index.insert(id.to_owned(), self.calls.len());
        self.calls.push(CallState { node, started: self.ts, answered: false, job, input: input.cloned() });
        Ok(node)
    }

    /// A subagent `<job>` of `call`, its agent type and title defaulting to the call's input.
    fn job_attrs(&self, id: &str, call: NodeId, input: Option<&J>, agent: Option<String>, title: Option<String>) -> Attrs {
        let from_input = |key| input.and_then(|i| str_field(i, key));
        let mut attrs: Attrs = vec![(attr::KIND, V::from(JobKind::Subagent)), (attr::ID, V::from(id)), (attr::CALL, V::from(call))];
        attrs.extend(agent.or_else(|| from_input("subagent_type")).map(|a| (attr::AGENT, V::from(a))));
        attrs.extend(title.or_else(|| from_input("description")).map(|t| (attr::TITLE, V::from(t))));
        let transcript = format!("subagents/agent-{id}.jsonl");
        attrs.extend(self.side_paths.contains(&transcript).then(|| (X_FILE, V::from(transcript))));
        attrs
    }

    fn compaction(&mut self, origin: &Origin, content: Option<&J>) -> Result<()> {
        self.close_turn()?;
        let (text, mut attrs, items) = encode(content, false);
        attrs.extend([(attr::COVERS, V::List(std::mem::take(&mut self.uncompacted))), (attr::CREATED, V::Int(self.ts))]);
        attrs.extend(self.pre_tokens.take().map(|t| (attr::TOKENS_BEFORE, V::Int(t))));
        attrs.extend(items.map(|items| (X_BLOCK, V::from(json!({ "content": items }).to_string()))));
        attrs.extend(origin.attrs(None));
        self.w().insert(self.root, E::Compaction, attrs, text)?;
        Ok(())
    }

    fn finish(mut self, files: Vec<(String, Vec<u8>)>) -> Result<()> {
        self.close_turn()?;
        let w = self.w();
        for call in self.calls.iter().filter(|c| !c.answered) {
            w.set(call.node, attr::STATUS, CallStatus::Cancelled)?;
            if let Some(job) = call.job {
                w.set(job, attr::STATUS, JobStatus::Cancelled)?;
            }
        }
        for (path, bytes) in files {
            let mut attrs: Attrs = vec![(attr::HARNESS, V::from(HARNESS)), (attr::TYPE, V::from("file")), (X_PATH, V::from(path))];
            let text = String::from_utf8(bytes).unwrap_or_else(|err| {
                attrs.push((attr::RAW, V::Bytes(err.into_bytes())));
                String::new()
            });
            w.insert(self.root, E::Ext, attrs, text)?;
        }
        if let Some(model) = self.model.take() {
            w.set(self.root, attr::MODEL, model)?;
        }
        if let Some(title) = self.custom_title.take().or_else(|| self.summary.take()) {
            w.set(self.root, attr::TITLE, title)?;
        }
        Ok(())
    }
}

fn message_attrs(role: Role, id: Option<&str>) -> Attrs {
    let mut attrs: Attrs = vec![(attr::ROLE, V::from(role)), (attr::STATUS, V::from(MessageStatus::Done))];
    attrs.extend(id.map(|id| (attr::ID, V::from(id))));
    attrs
}

/// The text of a block of type `ty` holding it in the field named `ty`.
fn block_text<'j>(block: &'j J, ty: &str) -> Option<&'j str> {
    (block["type"] == ty).then(|| block[ty].as_str()).flatten()
}

/// One line for the collapsed call row, from the argument that best names the call.
fn call_title(input: Option<&J>) -> Option<String> {
    const KEYS: [&str; 9] = ["description", "command", "file_path", "notebook_path", "path", "pattern", "url", "query", "prompt"];
    let input = input?.as_object()?;
    let text = KEYS.iter().find_map(|key| input.get(*key)?.as_str().filter(|s| !s.trim().is_empty()))?;
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    Some(first.chars().take(120).collect())
}

/// Writes `dom` as a transcript at `out` (a .jsonl path) plus its side files
/// under `out` without extension. Imported elements are grouped by line and
/// rebuilt from their envelope in line order; app-made elements follow as
/// fresh lines. Usage, jobs, decisions, todos and turns write nothing.
pub fn export(dom: &Dom, out: &Path) -> Result<()> {
    let root = dom.root().ok_or_else(|| AdapterError::Format("the session DOM is empty".to_owned()))?;
    let mut lines: BTreeMap<i64, Vec<(i64, usize, &Node)>> = BTreeMap::new();
    let (mut fresh, mut files) = (Vec::new(), Vec::new());
    for (order, node) in dom.iter().enumerate() {
        let line = node.int_attr(X_LINE);
        let entry = (node.int_attr(X_PART).unwrap_or(-1), order, node);
        match node.kind {
            E::Ext if node.str_attr(attr::HARNESS) == Some(HARNESS) => match (node.str_attr(X_PATH), line) {
                (Some(path), _) => files.push((path, node.attr(attr::RAW).and_then(V::as_bytes).unwrap_or(node.text.as_bytes()))),
                (None, Some(line)) => lines.entry(line).or_default().push(entry),
                (None, None) => {}
            },
            E::Message | E::Call | E::Result | E::Compaction => match line {
                Some(line) => lines.entry(line).or_default().push(entry),
                None => fresh.push(node),
            },
            _ => {}
        }
    }
    let mut writer = Writer { dom, root, out: String::new(), last_uuid: None, call_lines: HashMap::new(), call_ids: HashMap::new(), assistant: None };
    for mut parts in lines.into_values() {
        parts.sort_by_key(|(part, order, _)| (*part, *order));
        writer.group(&parts.into_iter().map(|(_, _, node)| node).collect::<Vec<_>>())?;
    }
    for node in fresh {
        writer.fresh(node)?;
    }
    write_file(out, writer.out)?;
    if files.is_empty() {
        return Ok(());
    }
    let dir = side_dir(out).ok_or_else(|| AdapterError::Format(format!("{} has no extension to strip for side files", out.display())))?;
    for (path, bytes) in files {
        if path.is_empty() || !Path::new(path).components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(AdapterError::Format(format!("side file path `{path}` leaves the side directory")));
        }
        write_file(&dir.join(path), bytes)?;
    }
    Ok(())
}

/// Writes `bytes` to `path`, creating its folder.
fn write_file(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(std::fs::write(path, bytes)?)
}

/// JSON text of an element, or the text as a JSON string when it doesn't parse.
fn json_or_string(text: &str) -> J {
    serde_json::from_str(text).unwrap_or_else(|_| J::String(text.to_owned()))
}

/// The `x-claude-block` object of an element, or `{"type": default_type}`.
fn block_residual(node: &Node, default_type: &str) -> Map<String, J> {
    match node.str_attr(X_BLOCK).map(serde_json::from_str) {
        Some(Ok(J::Object(block))) => block,
        _ => Map::from_iter([("type".to_owned(), J::from(default_type))]),
    }
}

struct Writer<'d> {
    dom: &'d Dom,
    root: &'d Node,
    out: String,
    /// uuid of the last line in the conversation chain.
    last_uuid: Option<String>,
    /// uuid of the line each written call came from.
    call_lines: HashMap<NodeId, String>,
    /// Tool use ids generated for calls without `call_id`.
    call_ids: HashMap<NodeId, String>,
    /// Turn and `message.id` that the next fresh assistant line continues.
    assistant: Option<(NodeId, String)>,
}

impl Writer<'_> {
    /// Notes what later fresh lines chain to.
    fn track(&mut self, line: &J) {
        if let Some(uuid) = line["uuid"].as_str().filter(|_| line.get("parentUuid").is_some()) {
            self.last_uuid = Some(uuid.to_owned());
        }
        if line["type"] == "user" {
            self.assistant = None;
        }
    }

    fn push(&mut self, line: J) {
        self.track(&line);
        self.out.push_str(&line.to_string());
        self.out.push('\n');
    }

    /// Writes the elements made from one source line, sorted by block.
    fn group(&mut self, nodes: &[&Node]) -> Result<()> {
        let Some(head) = nodes.iter().find(|n| n.attr(X_ENVELOPE).is_some()) else {
            for node in nodes {
                match node.kind {
                    // A whole line kept verbatim.
                    E::Ext if node.attr(X_PART).is_none() => {
                        if let Ok(line) = serde_json::from_str::<J>(&node.text) {
                            self.track(&line);
                        }
                        self.out.push_str(&node.text);
                        self.out.push('\n');
                    }
                    // Blocks whose envelope element was removed.
                    E::Ext => {}
                    _ => self.fresh(node)?,
                }
            }
            return Ok(());
        };
        let mut line = match head.str_attr(X_ENVELOPE).map(serde_json::from_str) {
            None => Map::new(),
            Some(Ok(J::Object(line))) => line,
            Some(Ok(_)) => return Err(AdapterError::Format(format!("{X_ENVELOPE} isn't a JSON object"))),
            Some(Err(err)) => return Err(AdapterError::Format(format!("{X_ENVELOPE}: {err}"))),
        };
        let content = match nodes {
            [node] if node.attr(X_PART).is_none() => self.whole(node),
            _ => Some(J::Array(nodes.iter().map(|n| self.block(n)).collect())),
        };
        if let Some(content) = content
            && let J::Object(message) = line.entry("message").or_insert_with(|| json!({}))
        {
            message.insert("content".to_owned(), content);
        }
        if let Some(uuid) = line.get("uuid").and_then(J::as_str) {
            for node in nodes.iter().filter(|n| n.kind == E::Call) {
                self.call_lines.insert(node.id, uuid.to_owned());
            }
        }
        self.push(J::Object(line));
        Ok(())
    }

    /// `message.content` of a line whose content wasn't an array.
    fn whole(&self, node: &Node) -> Option<J> {
        match node.kind {
            E::Message => Some(J::String(node.text.clone())),
            E::Compaction => self.content(node, block_residual(node, "text").remove("content")),
            _ => Some(self.block(node)),
        }
    }

    /// The content block an element stands for.
    fn block(&self, node: &Node) -> J {
        match node.kind {
            E::Message => {
                let field = if node.str_attr(attr::KIND) == Some(MessageKind::Thinking.as_str()) { "thinking" } else { "text" };
                let mut block = block_residual(node, field);
                block.insert(field.to_owned(), J::String(node.text.clone()));
                J::Object(block)
            }
            E::Call => J::Object(self.tool_use(node)),
            E::Result => J::Object(self.tool_result(node)),
            E::Ext => json_or_string(&node.text),
            _ => self.whole(node).unwrap_or(J::Null),
        }
    }

    /// Rebuilds content from element text; `None` for absent content. Text
    /// that no longer matches its `texts` split becomes one text block.
    fn content(&self, node: &Node, items: Option<J>) -> Option<J> {
        let kind = node.str_attr(X_CONTENT);
        match kind {
            Some("absent") => None,
            _ if kind == Some(JSON) || (kind.is_none() && node.str_attr(attr::FORMAT) == Some(JSON)) => Some(json_or_string(&node.text)),
            Some(TEXTS) => {
                let items = match items {
                    Some(J::Array(items)) => items,
                    _ => Vec::new(),
                };
                let split = node.attr(X_SPLIT).and_then(V::as_list).unwrap_or_default();
                let parts = residual::split_parts(&node.text, "\n", split).filter(|parts| parts.len() == items.len());
                Some(J::Array(match parts {
                    Some(parts) => items.into_iter().zip(parts).map(|(mut item, part)| {
                        if let Some(object) = item.as_object_mut() {
                            object.insert("text".to_owned(), part.into());
                        }
                        item
                    }).collect(),
                    None => vec![json!({ "type": "text", "text": node.text })],
                }))
            }
            _ => Some(J::String(node.text.clone())),
        }
    }

    fn call_id(&self, call: NodeId) -> Option<String> {
        let own = self.dom.get(call).and_then(|c| c.str_attr(attr::CALL_ID)).map(str::to_owned);
        own.or_else(|| self.call_ids.get(&call).cloned())
    }

    fn tool_use(&self, node: &Node) -> Map<String, J> {
        let mut block = block_residual(node, "tool_use");
        if let Some(id) = self.call_id(node.id) {
            block.insert("id".to_owned(), J::String(id));
        }
        block.insert("name".to_owned(), node.str_attr(attr::TOOL).unwrap_or_default().into());
        if let Some(input) = self.dom.children_of_kind(node.id, E::Input).next() {
            block.insert("input".to_owned(), json_or_string(&input.text));
        } else if node.attr(X_LINE).is_none() {
            block.insert("input".to_owned(), json!({}));
        }
        block
    }

    fn tool_result(&self, node: &Node) -> Map<String, J> {
        let mut block = block_residual(node, "tool_result");
        let items = block.remove("content");
        if let Some(id) = self.call_id(node.parent) {
            block.insert("tool_use_id".to_owned(), J::String(id));
        }
        if let Some(content) = self.content(node, items) {
            block.insert("content".to_owned(), content);
        }
        if let Some(is_error) = node.bool_attr(attr::IS_ERROR) {
            block.insert("is_error".to_owned(), J::Bool(is_error));
        }
        block
    }

    /// Time for a fresh line: the element's own start, else the nearest enclosing one; a result uses its call's end.
    fn fresh_ts(&self, node: &Node) -> i64 {
        let ended = (node.kind == E::Result).then(|| self.dom.get(node.parent)?.int_attr(attr::ENDED)).flatten();
        let started = || std::iter::once(node).chain(self.dom.ancestors(node.id)).find_map(|n| n.int_attr(attr::STARTED).or_else(|| n.int_attr(attr::CREATED)));
        ended.or_else(started).unwrap_or(0)
    }

    /// A fresh assistant `message`; consecutive assistant lines of one turn share `message.id` like one API response.
    fn assistant_message(&mut self, node: &Node, block: Map<String, J>) -> J {
        let id = match &self.assistant {
            Some((turn, id)) if *turn == node.parent => id.clone(),
            _ => format!("msg_{}", Uuid::new_v4().simple()),
        };
        self.assistant = Some((node.parent, id.clone()));
        let mut message = json!({"id": id, "type": "message", "role": "assistant"});
        if let Some(model) = node.str_attr(attr::MODEL).or_else(|| self.root.str_attr(attr::MODEL)) {
            message["model"] = model.into();
        }
        message["content"] = J::Array(vec![J::Object(block)]);
        message
    }

    /// Writes an element the app created as a new Claude Code line.
    fn fresh(&mut self, node: &Node) -> Result<()> {
        let uuid = Uuid::new_v4().to_string();
        let mut extra = Map::new();
        let user = |content: J| json!({ "role": "user", "content": content });
        let (ty, message) = match node.kind {
            E::Message => match node.str_attr(attr::ROLE).and_then(Role::parse) {
                Some(Role::Assistant) => {
                    let J::Object(block) = self.block(node) else { return Ok(()) };
                    ("assistant", self.assistant_message(node, block))
                }
                role => {
                    if role == Some(Role::System) {
                        extra.insert("isMeta".to_owned(), J::Bool(true));
                    }
                    ("user", user(J::String(node.text.clone())))
                }
            },
            E::Call => {
                if node.str_attr(attr::CALL_ID).is_none() {
                    self.call_ids.insert(node.id, format!("toolu_{}", Uuid::new_v4().simple()));
                }
                self.call_lines.insert(node.id, uuid.clone());
                let block = self.tool_use(node);
                ("assistant", self.assistant_message(node, block))
            }
            E::Result => {
                if let Some(source) = self.call_lines.get(&node.parent) {
                    extra.insert("sourceToolAssistantUUID".to_owned(), J::String(source.clone()));
                }
                ("user", user(J::Array(vec![J::Object(self.tool_result(node))])))
            }
            E::Compaction => {
                extra.insert("isCompactSummary".to_owned(), J::Bool(true));
                ("user", user(J::String(node.text.clone())))
            }
            _ => return Ok(()),
        };
        let root = self.root;
        let session_id = root.str_attr(attr::NATIVE_ID).or_else(|| root.str_attr(attr::ID)).unwrap_or_default();
        let mut line = Map::new();
        line.insert("parentUuid".to_owned(), self.last_uuid.clone().map_or(J::Null, J::String));
        line.insert("isSidechain".to_owned(), J::Bool(false));
        line.insert("userType".to_owned(), J::from("external"));
        if let Some(cwd) = root.str_attr(attr::CWD) {
            line.insert("cwd".to_owned(), J::from(cwd));
        }
        line.insert("sessionId".to_owned(), J::from(session_id));
        line.insert("type".to_owned(), J::from(ty));
        line.insert("message".to_owned(), message);
        line.insert("uuid".to_owned(), J::String(uuid));
        line.insert("timestamp".to_owned(), J::String(time::format_iso_ms(self.fresh_ts(node))));
        line.extend(extra);
        self.push(J::Object(line));
        Ok(())
    }
}
