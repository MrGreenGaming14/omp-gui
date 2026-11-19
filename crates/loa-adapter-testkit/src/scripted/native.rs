//! The scripted harness's native session file: one JSON record per line.
//!
//! ```text
//! {"type":"session","ts":1700000000000,"id":"s-1","node":"n","cwd":"/w","model":"m"}
//! {"type":"prompt","ts":1700000001000,"id":"t-1","text":"list files","by":"tai"}
//! {"type":"thinking","text":"Use ls."}
//! {"type":"tool_call","ts":1700000002000,"id":"c-1","tool":"bash","args":{"command":"ls"}}
//! {"type":"ask","ts":1700000002100,"id":"r-1","kind":"approval","call":"c-1","body":"ls"}
//! {"type":"answer","ts":1700000003000,"id":"r-1","answer":["allow_once"],"by":"tai"}
//! {"type":"tool_result","ts":1700000004000,"id":"c-1","output":"a.txt","is_error":false}
//! {"type":"text","text":"One file.","model":"m"}
//! {"type":"usage","input_tokens":10,"output_tokens":5}
//! {"type":"turn_end","ts":1700000005000,"status":"done"}
//! ```
//!
//! Records of an unknown type become `<ext harness="scripted" type>` holding
//! the line, under the open call, the open turn or the session, so the export
//! writes them back in place.

use super::HARNESS;
use super::writer::{AskSpec, SessionHeader, Writer};
use loa_adapter::{AdapterError, Result};
use loa_core::{
    DecisionKind, DecisionStatus, Dom, ElementKind, MessageKind, MessageStatus, Node, NodeId, PatchSink, Role,
    TurnStatus, Value, attr,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

/// One line of the file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Session {
        ts: i64,
        id: String,
        node: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Prompt {
        ts: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
    },
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Thinking {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    ToolCall {
        ts: i64,
        id: String,
        tool: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        args: serde_json::Value,
    },
    ToolResult {
        ts: i64,
        id: String,
        output: String,
        is_error: bool,
    },
    Ask {
        ts: i64,
        id: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
        body: String,
    },
    Answer {
        ts: i64,
        id: String,
        answer: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Usage {
        input_tokens: i64,
        output_tokens: i64,
    },
    TurnEnd {
        ts: i64,
        status: String,
    },
}

const KNOWN: &[&str] = &[
    "session",
    "prompt",
    "text",
    "thinking",
    "tool_call",
    "tool_result",
    "ask",
    "answer",
    "usage",
    "turn_end",
];

fn format_err(line: usize, what: impl std::fmt::Display) -> AdapterError {
    AdapterError::Format(format!("line {line}: {what}"))
}

/// Import state: what is open while reading the file.
struct Reader<'a> {
    w: Writer<'a>,
    session: Option<NodeId>,
    turn: Option<NodeId>,
    /// `call_id` to (node, ended).
    calls: HashMap<String, (NodeId, bool)>,
    /// The latest call without a result.
    open_call: Option<NodeId>,
    /// `request_id` to node, while waiting.
    waiting: HashMap<String, NodeId>,
    ts: i64,
}

/// Reads a scripted session file into `sink`.
pub fn import(native: &Path, sink: &dyn PatchSink) -> Result<()> {
    let text = std::fs::read_to_string(native)?;
    let mut r = Reader {
        w: Writer::new(sink),
        session: None,
        turn: None,
        calls: HashMap::new(),
        open_call: None,
        waiting: HashMap::new(),
        ts: 0,
    };
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line).map_err(|e| format_err(n, e))?;
        let kind = value.get("type").and_then(serde_json::Value::as_str).unwrap_or_default();
        if KNOWN.contains(&kind) {
            let record: Record = serde_json::from_value(value).map_err(|e| format_err(n, e))?;
            r.record(n, record)?;
        } else {
            let parent = r.open_call.or(r.turn).or(r.session).ok_or_else(|| format_err(n, "a record before the session"))?;
            r.w.ext(parent, HARNESS, kind, line, r.ts)?;
        }
    }
    if r.session.is_none() {
        return Err(AdapterError::Format("the file has no session record".to_owned()));
    }
    Ok(())
}

impl Reader<'_> {
    fn turn(&self, n: usize) -> Result<NodeId> {
        self.turn.ok_or_else(|| format_err(n, "no turn is open"))
    }

    fn record(&mut self, n: usize, record: Record) -> Result<()> {
        if self.session.is_none() && !matches!(record, Record::Session { .. }) {
            return Err(format_err(n, "a record before the session"));
        }
        match record {
            Record::Session { ts, id, node, cwd, project, model } => {
                if self.session.is_some() {
                    return Err(format_err(n, "a second session record"));
                }
                self.ts = ts;
                let header = SessionHeader {
                    id: &id,
                    harness: HARNESS,
                    node: &node,
                    created: ts,
                    cwd: cwd.as_deref(),
                    project: project.as_deref(),
                    model: model.as_deref(),
                    ..SessionHeader::default()
                };
                self.session = Some(self.w.session(&header)?);
            }
            Record::Prompt { ts, id, text, by } => {
                if self.turn.is_some() {
                    return Err(format_err(n, "a prompt while a turn is open"));
                }
                self.ts = ts;
                let session = self.session.ok_or_else(|| format_err(n, "no session"))?;
                self.turn = Some(self.w.turn(session, id.as_deref(), &text, by.as_deref(), ts)?);
            }
            Record::Text { text, model } => {
                self.w.message(self.turn(n)?, false, model.as_deref(), MessageStatus::Done, &text, self.ts)?;
            }
            Record::Thinking { text, model } => {
                self.w.message(self.turn(n)?, true, model.as_deref(), MessageStatus::Done, &text, self.ts)?;
            }
            Record::ToolCall { ts, id, tool, title, mut args } => {
                self.ts = ts;
                if self.calls.contains_key(&id) {
                    return Err(format_err(n, format!("call {id} appears twice")));
                }
                args.sort_all_objects();
                let args = serde_json::to_string(&args).map_err(|e| format_err(n, e))?;
                let (call, _) = self.w.call(self.turn(n)?, &tool, &id, title.as_deref(), &args, ts)?;
                self.calls.insert(id, (call, false));
                self.open_call = Some(call);
            }
            Record::ToolResult { ts, id, output, is_error } => {
                self.ts = ts;
                let Some((call, ended)) = self.calls.get_mut(&id) else {
                    return Err(format_err(n, format!("a result for unknown call {id}")));
                };
                if *ended {
                    return Err(format_err(n, format!("call {id} has two results")));
                }
                *ended = true;
                let call = *call;
                self.w.call_end(call, &output, is_error, ts)?;
                if self.open_call == Some(call) {
                    self.open_call = None;
                }
            }
            Record::Ask { ts, id, kind, call, title, options, body } => {
                self.ts = ts;
                let turn = self.turn(n)?;
                let kind = DecisionKind::parse(&kind).ok_or_else(|| format_err(n, format!("unknown kind {kind}")))?;
                let call = match call {
                    Some(c) => Some(self.calls.get(&c).ok_or_else(|| format_err(n, format!("unknown call {c}")))?.0),
                    None => None,
                };
                let spec = AskSpec { kind, request_id: &id, title: title.as_deref(), options: &options, body: &body };
                let decision = self.w.ask(call.unwrap_or(turn), turn, call, &spec, ts)?;
                self.waiting.insert(id, decision);
            }
            Record::Answer { ts, id, answer, by, reason } => {
                self.ts = ts;
                let turn = self.turn(n)?;
                let decision =
                    self.waiting.remove(&id).ok_or_else(|| format_err(n, format!("no waiting decision {id}")))?;
                self.w.answered(decision, turn, &answer, by.as_deref(), reason.as_deref(), ts)?;
            }
            Record::Usage { input_tokens, output_tokens } => {
                self.w.usage(self.turn(n)?, input_tokens, output_tokens, self.ts)?;
            }
            Record::TurnEnd { ts, status } => {
                self.ts = ts;
                let turn = self.turn.take().ok_or_else(|| format_err(n, "no turn is open"))?;
                let status = TurnStatus::parse(&status)
                    .filter(|s| matches!(s, TurnStatus::Done | TurnStatus::Error | TurnStatus::Cancelled))
                    .ok_or_else(|| format_err(n, format!("{status} doesn't end a turn")))?;
                self.w.end_turn(turn, status, ts)?;
                self.open_call = None;
            }
        }
        Ok(())
    }
}

fn missing(node: &Node, what: &str) -> AdapterError {
    AdapterError::Format(format!("<{}> node {} has no {what}", node.kind, node.id))
}

fn str_attr(node: &Node, name: &str) -> Option<String> {
    node.str_attr(name).map(str::to_owned)
}

/// Writes `dom` as a scripted session file at `out`.
pub fn export(dom: &Dom, out: &Path) -> Result<()> {
    let root = dom.root().filter(|r| r.kind == ElementKind::Session).ok_or_else(|| {
        AdapterError::Format("the DOM has no <session> root".to_owned())
    })?;
    let mut lines = Vec::new();
    let mut e = Exporter { dom, out: &mut lines };
    e.record(&Record::Session {
        ts: root.int_attr(attr::CREATED).ok_or_else(|| missing(root, "created"))?,
        id: str_attr(root, attr::ID).ok_or_else(|| missing(root, "id"))?,
        node: str_attr(root, attr::NODE).ok_or_else(|| missing(root, "node"))?,
        cwd: str_attr(root, attr::CWD),
        project: str_attr(root, attr::PROJECT),
        model: str_attr(root, attr::MODEL),
    })?;
    for child in dom.children(root.id) {
        match child.kind {
            ElementKind::Turn => e.turn(child)?,
            ElementKind::Ext => e.raw(child),
            other => return Err(AdapterError::Format(format!("the scripted format has no place for <{other}>"))),
        }
    }
    let mut file = std::fs::File::create(out)?;
    file.write_all(&lines)?;
    Ok(())
}

struct Exporter<'a> {
    dom: &'a Dom,
    out: &'a mut Vec<u8>,
}

impl Exporter<'_> {
    fn record(&mut self, record: &Record) -> Result<()> {
        serde_json::to_writer(&mut *self.out, record).map_err(|e| AdapterError::Format(e.to_string()))?;
        self.out.push(b'\n');
        Ok(())
    }

    fn raw(&mut self, ext: &Node) {
        self.out.extend_from_slice(ext.text.as_bytes());
        self.out.push(b'\n');
    }

    fn turn(&mut self, turn: &Node) -> Result<()> {
        let mut prompted = false;
        for child in self.dom.children(turn.id) {
            match child.kind {
                ElementKind::Message if !prompted => {
                    if child.str_attr(attr::ROLE) != Some(Role::User.as_str()) {
                        return Err(AdapterError::Format(format!("turn {} doesn't start with a prompt", turn.id)));
                    }
                    prompted = true;
                    self.record(&Record::Prompt {
                        ts: turn.int_attr(attr::STARTED).ok_or_else(|| missing(turn, "started"))?,
                        id: str_attr(turn, attr::ID),
                        text: child.text.clone(),
                        by: str_attr(child, attr::BY),
                    })?;
                }
                ElementKind::Message => {
                    let text = child.text.clone();
                    let model = str_attr(child, attr::MODEL);
                    let record = if child.str_attr(attr::KIND) == Some(MessageKind::Thinking.as_str()) {
                        Record::Thinking { text, model }
                    } else {
                        Record::Text { text, model }
                    };
                    self.record(&record)?;
                }
                ElementKind::Call => self.call(child)?,
                ElementKind::Decision => self.decision(child)?,
                ElementKind::Usage => self.usage(child)?,
                ElementKind::Ext => self.raw(child),
                other => return Err(AdapterError::Format(format!("the scripted format has no place for <{other}>"))),
            }
        }
        let status = turn.str_attr(attr::STATUS).and_then(TurnStatus::parse);
        if let Some(status @ (TurnStatus::Done | TurnStatus::Error | TurnStatus::Cancelled)) = status {
            self.record(&Record::TurnEnd {
                ts: turn.int_attr(attr::ENDED).ok_or_else(|| missing(turn, "ended"))?,
                status: status.as_str().to_owned(),
            })?;
        }
        Ok(())
    }

    fn call(&mut self, call: &Node) -> Result<()> {
        let id = str_attr(call, attr::CALL_ID).ok_or_else(|| missing(call, "call_id"))?;
        let input = self.dom.children_of_kind(call.id, ElementKind::Input).next().ok_or_else(|| missing(call, "input"))?;
        let args = serde_json::from_str(&input.text).map_err(|e| AdapterError::Format(e.to_string()))?;
        self.record(&Record::ToolCall {
            ts: call.int_attr(attr::STARTED).ok_or_else(|| missing(call, "started"))?,
            id: id.clone(),
            tool: str_attr(call, attr::TOOL).ok_or_else(|| missing(call, "tool"))?,
            title: str_attr(call, attr::TITLE),
            args,
        })?;
        for child in self.dom.children(call.id) {
            match child.kind {
                ElementKind::Input => {}
                ElementKind::Decision => self.decision(child)?,
                ElementKind::Result => self.record(&Record::ToolResult {
                    ts: call.int_attr(attr::ENDED).ok_or_else(|| missing(call, "ended"))?,
                    id: id.clone(),
                    output: child.text.clone(),
                    is_error: child.bool_attr(attr::IS_ERROR).unwrap_or(false),
                })?,
                ElementKind::Usage => self.usage(child)?,
                ElementKind::Ext => self.raw(child),
                other => return Err(AdapterError::Format(format!("the scripted format has no place for <{other}>"))),
            }
        }
        Ok(())
    }

    fn decision(&mut self, decision: &Node) -> Result<()> {
        let id = str_attr(decision, attr::REQUEST_ID).ok_or_else(|| missing(decision, "request_id"))?;
        let call = match decision.int_attr(attr::CALL) {
            Some(call) => {
                let node = u64::try_from(call).ok().and_then(|c| self.dom.get(NodeId(c)));
                Some(node.and_then(|n| str_attr(n, attr::CALL_ID)).ok_or_else(|| missing(decision, "call it names"))?)
            }
            None => None,
        };
        self.record(&Record::Ask {
            ts: decision.int_attr(attr::CREATED).ok_or_else(|| missing(decision, "created"))?,
            id: id.clone(),
            kind: str_attr(decision, attr::KIND).ok_or_else(|| missing(decision, "kind"))?,
            call,
            title: str_attr(decision, attr::TITLE),
            options: decision.attr(attr::OPTIONS).map(|o| o.str_items().map(str::to_owned).collect()).unwrap_or_default(),
            body: decision.text.clone(),
        })?;
        if decision.str_attr(attr::STATUS) == Some(DecisionStatus::Answered.as_str()) {
            self.record(&Record::Answer {
                ts: decision.int_attr(attr::ENDED).ok_or_else(|| missing(decision, "ended"))?,
                id,
                answer: decision.attr(attr::ANSWER).map(|a| a.str_items().map(str::to_owned).collect()).unwrap_or_default(),
                by: str_attr(decision, attr::BY),
                reason: str_attr(decision, attr::REASON),
            })?;
        }
        Ok(())
    }

    fn usage(&mut self, usage: &Node) -> Result<()> {
        let get = |name| usage.attr(name).and_then(Value::as_int).ok_or_else(|| missing(usage, name));
        self.record(&Record::Usage { input_tokens: get(attr::INPUT_TOKENS)?, output_tokens: get(attr::OUTPUT_TOKENS)? })
    }
}
