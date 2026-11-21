//! Transcript to DOM.

use super::content::{self, Encoded};
use super::{
    EXT_BLOCK, EXT_FILE, X_BLOCK, X_CONTENT, X_ENVELOPE, X_FILE, X_LINE, X_PART, X_PATH, X_SPLIT, take_field, time,
};
use crate::HARNESS;
use loa_adapter::Result;
use loa_core::{
    CallStatus, Draft, ElementKind, JobKind, JobStatus, MessageKind, MessageStatus, NodeId, PatchSink, Role,
    SCHEMA_VERSION, TurnStatus, Value, attr,
};
use serde_json::{Map, Value as J};
use std::collections::{HashMap, HashSet};
use std::path::Path;

type Attrs = Vec<(&'static str, Value)>;

/// One non-blank transcript line.
struct Line {
    index: i64,
    text: String,
    /// `None` when the line isn't JSON.
    json: Option<J>,
}

/// A file of the side directory.
struct SideFile {
    /// Relative path with `/` separators.
    path: String,
    bytes: Vec<u8>,
}

/// What `subagents/agent-<id>.meta.json` says about one subagent.
struct Subagent {
    agent_id: String,
    agent_type: Option<String>,
    description: Option<String>,
    /// The subagent transcript relative to the side directory, when present.
    file: Option<String>,
}

pub(super) fn run(path: &Path, sink: &dyn PatchSink) -> Result<()> {
    let source = std::fs::read_to_string(path)?;
    let lines: Vec<Line> = source
        .split('\n')
        .filter(|text| !text.trim().is_empty())
        .enumerate()
        .map(|(index, text)| Line {
            // A file never holds 2^63 lines.
            index: index as i64,
            text: text.to_owned(),
            json: serde_json::from_str(text).ok(),
        })
        .collect();
    let files = match super::side_dir(path) {
        Some(dir) => side_files(&dir)?,
        None => Vec::new(),
    };
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let side_paths = files.iter().map(|f| f.path.clone()).collect();
    let mut importer = Importer::new(sink, &lines, &stem, subagents(&files), side_paths)?;
    for line in &lines {
        importer.line(line)?;
    }
    importer.finish(files)
}

/// Every regular file under `dir`, sorted by path. Empty when `dir` doesn't exist.
fn side_files(dir: &Path) -> Result<Vec<SideFile>> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return Ok(files);
    }
    let mut pending = vec![(dir.to_path_buf(), String::new())];
    while let Some((path, prefix)) = pending.pop() {
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push((entry.path(), rel));
            } else if kind.is_file() {
                files.push(SideFile { path: rel, bytes: std::fs::read(entry.path())? });
            }
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Subagents by the tool use id that started them.
fn subagents(files: &[SideFile]) -> HashMap<String, Subagent> {
    let mut found = HashMap::new();
    for file in files {
        let Some(agent_id) =
            file.path.strip_prefix("subagents/agent-").and_then(|rest| rest.strip_suffix(".meta.json"))
        else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<J>(&file.bytes) else { continue };
        let Some(tool_use_id) = meta.get("toolUseId").and_then(J::as_str) else { continue };
        let transcript = format!("subagents/agent-{agent_id}.jsonl");
        found.insert(
            tool_use_id.to_owned(),
            Subagent {
                agent_id: agent_id.to_owned(),
                agent_type: str_field(&meta, "agentType"),
                description: str_field(&meta, "description"),
                file: files.iter().any(|f| f.path == transcript).then_some(transcript),
            },
        );
    }
    found
}

fn str_field(value: &J, name: &str) -> Option<String> {
    value.get(name).and_then(J::as_str).map(str::to_owned)
}

fn int(value: usize) -> Value {
    // Block indexes and node ids stay far below 2^63.
    Value::Int(value as i64)
}

fn node_int(node: NodeId) -> Value {
    // Node ids are allocated one by one from 1 and stay far below 2^63.
    Value::Int(node.0 as i64)
}

/// The source of the elements made from one line.
struct Origin {
    index: i64,
    /// JSON text of the line without `message.content`.
    envelope: String,
}

impl Origin {
    fn new(index: i64, line: &Map<String, J>) -> Self {
        let mut envelope = line.clone();
        if let Some(J::Object(message)) = envelope.get_mut("message") {
            take_field(message, "content");
        }
        Self { index, envelope: J::Object(envelope).to_string() }
    }

    /// Attributes tying an element to block `part` of the line (`None` when
    /// the content isn't an array). The first element carries the envelope.
    fn attrs(&self, part: Option<usize>) -> Attrs {
        let mut attrs = vec![(X_LINE, Value::Int(self.index))];
        if let Some(part) = part {
            attrs.push((X_PART, int(part)));
        }
        if part.unwrap_or(0) == 0 {
            attrs.push((X_ENVELOPE, Value::Str(self.envelope.clone())));
        }
        attrs
    }
}

/// `block` without `fields`, as JSON text.
fn leftover(block: &J, fields: &[&str]) -> String {
    let mut rest = block.clone();
    if let Some(object) = rest.as_object_mut() {
        for field in fields {
            take_field(object, field);
        }
    }
    rest.to_string()
}

fn content_attrs(attrs: &mut Attrs, encoded: &Encoded) {
    attrs.push((X_CONTENT, Value::from(encoded.kind)));
    if encoded.kind == content::TEXTS {
        attrs.push((X_SPLIT, Value::List(encoded.split.iter().map(|n| Value::Int(*n)).collect())));
    }
}

/// One-line summary of a call's arguments for the collapsed row.
fn call_title(input: Option<&J>) -> Option<String> {
    const KEYS: [&str; 9] =
        ["description", "command", "file_path", "notebook_path", "path", "pattern", "url", "query", "prompt"];
    let input = input?.as_object()?;
    let text = KEYS.iter().find_map(|key| input.get(*key)?.as_str().filter(|s| !s.trim().is_empty()))?;
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().trim();
    Some(first.chars().take(120).collect())
}

struct OpenTurn {
    node: NodeId,
    last_ts: i64,
}

struct CallState {
    node: NodeId,
    started: i64,
    answered: bool,
    job: Option<NodeId>,
    description: Option<String>,
    subagent_type: Option<String>,
}

struct Importer<'a> {
    sink: &'a dyn PatchSink,
    root: NodeId,
    /// Every conversation line is a sidechain: the file is a subagent's.
    subagent_file: bool,
    subagents: HashMap<String, Subagent>,
    /// Relative paths of every side file.
    side_paths: HashSet<String>,
    /// Timestamp of the current line, or the last one seen.
    ts: i64,
    turn: Option<OpenTurn>,
    /// Turns since the last compaction.
    uncompacted: Vec<Value>,
    calls: Vec<CallState>,
    call_index: HashMap<String, usize>,
    /// Assistant `message.id`s seen, so usage lands on the first line only.
    messages: HashSet<String>,
    model: Option<String>,
    custom_title: Option<String>,
    summary: Option<String>,
    /// `compactMetadata.preTokens` of the last compact boundary.
    pre_tokens: Option<i64>,
}

impl<'a> Importer<'a> {
    fn new(
        sink: &'a dyn PatchSink,
        lines: &[Line],
        stem: &str,
        subagents: HashMap<String, Subagent>,
        side_paths: HashSet<String>,
    ) -> Result<Self> {
        let objects = || lines.iter().filter_map(|l| l.json.as_ref());
        let first_str = |name: &str| objects().find_map(|j| j.get(name)?.as_str().map(str::to_owned));
        let session_id = first_str("sessionId");
        let created = objects().find_map(|j| time::parse_iso_ms(j.get("timestamp")?.as_str()?)).unwrap_or(0);
        let mut conversation = objects()
            .filter(|j| matches!(j.get("type").and_then(J::as_str), Some("user" | "assistant")))
            .peekable();
        let subagent_file =
            conversation.peek().is_some() && conversation.all(|j| j.get("isSidechain") == Some(&J::Bool(true)));

        let mut attrs: Attrs = vec![
            (attr::ID, Value::from(session_id.clone().unwrap_or_else(|| stem.to_owned()))),
            (attr::HARNESS, Value::from(HARNESS)),
            (attr::NODE, Value::from("")),
            (attr::CREATED, Value::Int(created)),
            (attr::SCHEMA, Value::Int(i64::from(SCHEMA_VERSION))),
        ];
        if let Some(id) = session_id {
            attrs.push((attr::NATIVE_ID, Value::from(id)));
        }
        if let Some(cwd) = first_str("cwd") {
            attrs.push((attr::CWD, Value::from(cwd)));
        }
        let root = sink.commit_at(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""), created)?.node;
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

    fn insert(&self, parent: NodeId, kind: ElementKind, attrs: Attrs, text: impl Into<String>) -> Result<NodeId> {
        Ok(self.sink.commit_at(Draft::insert(parent, kind, attrs, text), self.ts)?.node)
    }

    fn set(&self, node: NodeId, name: &str, value: impl Into<Value>) -> Result<()> {
        self.sink.commit_at(Draft::set(node, name, value), self.ts)?;
        Ok(())
    }

    fn line(&mut self, line: &Line) -> Result<()> {
        if let Some(ts) = line.json.as_ref().and_then(|j| time::parse_iso_ms(j.get("timestamp")?.as_str()?)) {
            self.ts = ts;
        }
        match &line.json {
            Some(J::Object(object)) => self.object(line, object)?,
            _ => self.line_ext(line, None)?,
        }
        if let Some(turn) = &mut self.turn {
            turn.last_ts = self.ts;
        }
        Ok(())
    }

    fn object(&mut self, line: &Line, object: &Map<String, J>) -> Result<()> {
        let ty = object.get("type").and_then(J::as_str);
        let own_line = self.subagent_file || object.get("isSidechain") != Some(&J::Bool(true));
        match ty {
            Some("user") if own_line => self.user(line, object),
            Some("assistant") if own_line => self.assistant(line, object),
            Some("custom-title") => {
                if let Some(title) = object.get("customTitle").and_then(J::as_str) {
                    self.custom_title = Some(title.to_owned());
                }
                self.line_ext(line, ty)
            }
            Some("summary") => {
                if let Some(summary) = object.get("summary").and_then(J::as_str) {
                    self.summary = Some(summary.to_owned());
                }
                self.line_ext(line, ty)
            }
            Some("system") => {
                if object.get("subtype").and_then(J::as_str) == Some("compact_boundary") {
                    self.pre_tokens = object.get("compactMetadata").and_then(|m| m.get("preTokens")?.as_i64());
                }
                self.line_ext(line, ty)
            }
            _ => self.line_ext(line, ty),
        }
    }

    /// Where an ext goes: the current turn, or the session before the first turn.
    fn ext_parent(&self) -> NodeId {
        self.turn.as_ref().map_or(self.root, |t| t.node)
    }

    /// Keeps a whole line verbatim.
    fn line_ext(&self, line: &Line, ty: Option<&str>) -> Result<()> {
        let mut attrs: Attrs = vec![(attr::HARNESS, Value::from(HARNESS)), (X_LINE, Value::Int(line.index))];
        if let Some(ty) = ty {
            attrs.push((attr::TYPE, Value::from(ty)));
        }
        self.insert(self.ext_parent(), ElementKind::Ext, attrs, line.text.as_str())?;
        Ok(())
    }

    /// Keeps one content block verbatim.
    fn block_ext(&self, parent: NodeId, origin: &Origin, part: usize, block: &J) -> Result<NodeId> {
        let mut attrs: Attrs = vec![(attr::HARNESS, Value::from(HARNESS)), (attr::TYPE, Value::from(EXT_BLOCK))];
        attrs.extend(origin.attrs(Some(part)));
        self.insert(parent, ElementKind::Ext, attrs, block.to_string())
    }

    fn open_turn(&mut self, id: Option<&str>) -> Result<NodeId> {
        self.close_turn()?;
        let mut attrs: Attrs =
            vec![(attr::STATUS, Value::from(TurnStatus::Done)), (attr::STARTED, Value::Int(self.ts))];
        if let Some(id) = id {
            attrs.push((attr::ID, Value::from(id)));
        }
        let node = self.insert(self.root, ElementKind::Turn, attrs, "")?;
        self.uncompacted.push(node_int(node));
        self.turn = Some(OpenTurn { node, last_ts: self.ts });
        Ok(node)
    }

    fn close_turn(&mut self) -> Result<()> {
        if let Some(turn) = self.turn.take() {
            self.set(turn.node, attr::ENDED, turn.last_ts)?;
        }
        Ok(())
    }

    fn current_turn(&mut self) -> Result<NodeId> {
        match &self.turn {
            Some(turn) => Ok(turn.node),
            None => self.open_turn(None),
        }
    }

    fn user(&mut self, line: &Line, object: &Map<String, J>) -> Result<()> {
        let content = object.get("message").and_then(|m| m.get("content"));
        let blocks = match content {
            Some(J::String(_)) => None,
            Some(J::Array(blocks)) if !blocks.is_empty() => Some(blocks),
            _ => return self.line_ext(line, Some("user")),
        };
        let flag = |name: &str| object.get(name) == Some(&J::Bool(true));
        let uuid = object.get("uuid").and_then(J::as_str);
        let origin = Origin::new(line.index, object);
        if flag("isCompactSummary") {
            return self.compaction(&origin, content);
        }
        if let Some(blocks) = blocks
            && blocks.iter().any(|b| b.get("type").and_then(J::as_str) == Some("tool_result"))
        {
            return self.results(&origin, uuid, object, blocks);
        }
        let (role, turn) = if flag("isMeta") {
            (Role::System, self.current_turn()?)
        } else {
            (Role::User, self.open_turn(uuid)?)
        };
        match (content, blocks) {
            (Some(J::String(text)), _) => {
                let mut attrs = message_attrs(role, uuid);
                attrs.extend(origin.attrs(None));
                self.insert(turn, ElementKind::Message, attrs, text.as_str())?;
            }
            (_, Some(blocks)) => {
                for (part, block) in blocks.iter().enumerate() {
                    match block_text(block, "text") {
                        Some(text) => {
                            let mut attrs = message_attrs(role, uuid);
                            attrs.push((X_BLOCK, Value::from(leftover(block, &["text"]))));
                            attrs.extend(origin.attrs(Some(part)));
                            self.insert(turn, ElementKind::Message, attrs, text)?;
                        }
                        None => {
                            self.block_ext(turn, &origin, part, block)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn results(&mut self, origin: &Origin, uuid: Option<&str>, object: &Map<String, J>, blocks: &[J]) -> Result<()> {
        let result_count = blocks.iter().filter(|b| b.get("type").and_then(J::as_str) == Some("tool_result")).count();
        let agent = object.get("toolUseResult").filter(|_| result_count == 1);
        for (part, block) in blocks.iter().enumerate() {
            let call = (block.get("type").and_then(J::as_str) == Some("tool_result"))
                .then(|| block.get("tool_use_id")?.as_str())
                .flatten()
                .and_then(|id| self.call_index.get(id).copied());
            if let Some(call) = call {
                self.result(origin, part, block, call, agent)?;
            } else if let Some(text) = block_text(block, "text") {
                let turn = self.current_turn()?;
                let mut attrs = message_attrs(Role::User, uuid);
                attrs.push((X_BLOCK, Value::from(leftover(block, &["text"]))));
                attrs.extend(origin.attrs(Some(part)));
                self.insert(turn, ElementKind::Message, attrs, text)?;
            } else {
                self.block_ext(self.ext_parent(), origin, part, block)?;
            }
        }
        Ok(())
    }

    fn result(&mut self, origin: &Origin, part: usize, block: &J, call: usize, agent: Option<&J>) -> Result<()> {
        let encoded = content::encode(block.get("content"));
        let is_error = block.get("is_error").and_then(J::as_bool);
        let mut rest = block.clone();
        if let Some(object) = rest.as_object_mut() {
            take_field(object, "tool_use_id");
            take_field(object, "content");
            if is_error.is_some() {
                take_field(object, "is_error");
            }
            if encoded.kind == content::TEXTS {
                object.insert("content".to_owned(), J::Array(encoded.items.clone()));
            }
        }
        let mut attrs: Attrs = Vec::new();
        if encoded.kind == content::JSON {
            attrs.push((attr::FORMAT, Value::from("json")));
        }
        if let Some(is_error) = is_error {
            attrs.push((attr::IS_ERROR, Value::Bool(is_error)));
        }
        content_attrs(&mut attrs, &encoded);
        attrs.push((X_BLOCK, Value::from(rest.to_string())));
        attrs.extend(origin.attrs(Some(part)));
        let call_node = self.calls[call].node;
        self.insert(call_node, ElementKind::Result, attrs, encoded.text)?;

        let failed = is_error == Some(true);
        let status = if failed { CallStatus::Error } else { CallStatus::Done };
        self.set(call_node, attr::STATUS, status)?;
        self.set(call_node, attr::ENDED, self.ts)?;
        self.calls[call].answered = true;
        let job_status = if failed { JobStatus::Error } else { JobStatus::Done };
        match self.calls[call].job {
            Some(job) => {
                self.set(job, attr::STATUS, job_status)?;
                self.set(job, attr::ENDED, self.ts)?;
            }
            None => {
                if let Some(agent_id) = agent.and_then(|a| a.get("agentId")?.as_str()) {
                    let state = &self.calls[call];
                    let agent_type = agent.and_then(|a| str_field(a, "agentType")).or(state.subagent_type.clone());
                    let mut attrs: Attrs = vec![
                        (attr::KIND, Value::from(JobKind::Subagent)),
                        (attr::STATUS, Value::from(job_status)),
                        (attr::ID, Value::from(agent_id)),
                        (attr::CALL, node_int(call_node)),
                        (attr::STARTED, Value::Int(state.started)),
                        (attr::ENDED, Value::Int(self.ts)),
                    ];
                    if let Some(agent_type) = agent_type {
                        attrs.push((attr::AGENT, Value::from(agent_type)));
                    }
                    if let Some(title) = &state.description {
                        attrs.push((attr::TITLE, Value::from(title.as_str())));
                    }
                    let transcript = format!("subagents/agent-{agent_id}.jsonl");
                    if self.side_paths.contains(&transcript) {
                        attrs.push((X_FILE, Value::from(transcript)));
                    }
                    let job = self.insert(call_node, ElementKind::Job, attrs, "")?;
                    self.calls[call].job = Some(job);
                }
            }
        }
        Ok(())
    }

    fn assistant(&mut self, line: &Line, object: &Map<String, J>) -> Result<()> {
        let message = object.get("message").and_then(J::as_object);
        let Some(blocks) = message.and_then(|m| m.get("content")?.as_array()).filter(|b| !b.is_empty()) else {
            return self.line_ext(line, Some("assistant"));
        };
        let turn = self.current_turn()?;
        let model = message.and_then(|m| m.get("model")?.as_str());
        if let Some(model) = model {
            self.model = Some(model.to_owned());
        }
        let uuid = object.get("uuid").and_then(J::as_str);
        let origin = Origin::new(line.index, object);
        let first_line = match message.and_then(|m| m.get("id")?.as_str()) {
            Some(id) => self.messages.insert(id.to_owned()),
            None => true,
        };
        let mut usage_parent = turn;
        for (part, block) in blocks.iter().enumerate() {
            let ty = block.get("type").and_then(J::as_str);
            let text = match ty {
                Some("text") => block_text(block, "text").map(|t| (t, "text", None)),
                Some("thinking") => block_text(block, "thinking").map(|t| (t, "thinking", Some(MessageKind::Thinking))),
                _ => None,
            };
            let call = (ty == Some("tool_use"))
                .then(|| Some((block.get("id")?.as_str()?, block.get("name")?.as_str()?)))
                .flatten();
            let (node, usage_ok) = if let Some((text, field, kind)) = text {
                let mut attrs = message_attrs(Role::Assistant, uuid);
                if let Some(kind) = kind {
                    attrs.push((attr::KIND, Value::from(kind)));
                }
                if let Some(model) = model {
                    attrs.push((attr::MODEL, Value::from(model)));
                }
                attrs.push((X_BLOCK, Value::from(leftover(block, &[field]))));
                attrs.extend(origin.attrs(Some(part)));
                (self.insert(turn, ElementKind::Message, attrs, text)?, true)
            } else if let Some((id, name)) = call {
                (self.call(turn, &origin, part, block, id, name)?, true)
            } else {
                (self.block_ext(turn, &origin, part, block)?, false)
            };
            if part == 0 && usage_ok {
                usage_parent = node;
            }
        }
        if first_line && let Some(usage) = message.and_then(|m| m.get("usage")?.as_object()) {
            let mut attrs: Attrs = Vec::new();
            for (name, key) in [
                (attr::INPUT_TOKENS, "input_tokens"),
                (attr::OUTPUT_TOKENS, "output_tokens"),
                (attr::CACHE_READ_TOKENS, "cache_read_input_tokens"),
                (attr::CACHE_WRITE_TOKENS, "cache_creation_input_tokens"),
            ] {
                if let Some(n) = usage.get(key).and_then(J::as_i64) {
                    attrs.push((name, Value::Int(n)));
                }
            }
            if let Some(model) = model {
                attrs.push((attr::MODEL, Value::from(model)));
            }
            self.insert(usage_parent, ElementKind::Usage, attrs, "")?;
        }
        Ok(())
    }

    fn call(&mut self, turn: NodeId, origin: &Origin, part: usize, block: &J, id: &str, name: &str) -> Result<NodeId> {
        let input = block.get("input");
        let mut attrs: Attrs = vec![
            (attr::TOOL, Value::from(name)),
            (attr::CALL_ID, Value::from(id)),
            (attr::STATUS, Value::from(CallStatus::Running)),
            (attr::STARTED, Value::Int(self.ts)),
        ];
        if let Some(title) = call_title(input) {
            attrs.push((attr::TITLE, Value::from(title)));
        }
        attrs.push((X_BLOCK, Value::from(leftover(block, &["id", "name", "input"]))));
        attrs.extend(origin.attrs(Some(part)));
        let node = self.insert(turn, ElementKind::Call, attrs, "")?;
        if let Some(input) = input {
            self.insert(node, ElementKind::Input, vec![(attr::FORMAT, Value::from("json"))], input.to_string())?;
        }
        let mut job = None;
        if let Some(subagent) = self.subagents.get(id) {
            let mut attrs: Attrs = vec![
                (attr::KIND, Value::from(JobKind::Subagent)),
                (attr::STATUS, Value::from(JobStatus::Running)),
                (attr::ID, Value::from(subagent.agent_id.as_str())),
                (attr::CALL, node_int(node)),
                (attr::STARTED, Value::Int(self.ts)),
            ];
            let agent_type = subagent.agent_type.clone().or_else(|| input.and_then(|i| str_field(i, "subagent_type")));
            if let Some(agent_type) = agent_type {
                attrs.push((attr::AGENT, Value::from(agent_type)));
            }
            let title = subagent.description.clone().or_else(|| input.and_then(|i| str_field(i, "description")));
            if let Some(title) = title {
                attrs.push((attr::TITLE, Value::from(title)));
            }
            if let Some(file) = &subagent.file {
                attrs.push((X_FILE, Value::from(file.as_str())));
            }
            job = Some(self.insert(node, ElementKind::Job, attrs, "")?);
        }
        self.call_index.insert(id.to_owned(), self.calls.len());
        self.calls.push(CallState {
            node,
            started: self.ts,
            answered: false,
            job,
            description: input.and_then(|i| str_field(i, "description")),
            subagent_type: input.and_then(|i| str_field(i, "subagent_type")),
        });
        Ok(node)
    }

    fn compaction(&mut self, origin: &Origin, content: Option<&J>) -> Result<()> {
        self.close_turn()?;
        let encoded = content::encode(content);
        let mut attrs: Attrs = vec![
            (attr::COVERS, Value::List(std::mem::take(&mut self.uncompacted))),
            (attr::CREATED, Value::Int(self.ts)),
        ];
        if let Some(tokens) = self.pre_tokens.take() {
            attrs.push((attr::TOKENS_BEFORE, Value::Int(tokens)));
        }
        content_attrs(&mut attrs, &encoded);
        if encoded.kind == content::TEXTS {
            let items = serde_json::json!({ "content": encoded.items });
            attrs.push((X_BLOCK, Value::from(items.to_string())));
        }
        attrs.extend(origin.attrs(None));
        self.insert(self.root, ElementKind::Compaction, attrs, encoded.text)?;
        Ok(())
    }

    fn finish(mut self, files: Vec<SideFile>) -> Result<()> {
        self.close_turn()?;
        for call in self.calls.iter().filter(|c| !c.answered) {
            self.set(call.node, attr::STATUS, CallStatus::Cancelled)?;
            if let Some(job) = call.job {
                self.set(job, attr::STATUS, JobStatus::Cancelled)?;
            }
        }
        for file in files {
            let mut attrs: Attrs = vec![
                (attr::HARNESS, Value::from(HARNESS)),
                (attr::TYPE, Value::from(EXT_FILE)),
                (X_PATH, Value::from(file.path)),
            ];
            let text = match String::from_utf8(file.bytes) {
                Ok(text) => text,
                Err(err) => {
                    attrs.push((attr::RAW, Value::Bytes(err.into_bytes())));
                    String::new()
                }
            };
            self.insert(self.root, ElementKind::Ext, attrs, text)?;
        }
        if let Some(model) = self.model.take() {
            self.set(self.root, attr::MODEL, model)?;
        }
        if let Some(title) = self.custom_title.take().or_else(|| self.summary.take()) {
            self.set(self.root, attr::TITLE, title)?;
        }
        Ok(())
    }
}

fn message_attrs(role: Role, id: Option<&str>) -> Attrs {
    let mut attrs: Attrs = vec![(attr::ROLE, Value::from(role)), (attr::STATUS, Value::from(MessageStatus::Done))];
    if let Some(id) = id {
        attrs.push((attr::ID, Value::from(id)));
    }
    attrs
}

/// The text of a `text` or `thinking` block: the string field named like the
/// block type, when the block has type `ty`.
fn block_text<'j>(block: &'j J, ty: &str) -> Option<&'j str> {
    if block.get("type")?.as_str()? != ty {
        return None;
    }
    block.get(ty)?.as_str()
}
