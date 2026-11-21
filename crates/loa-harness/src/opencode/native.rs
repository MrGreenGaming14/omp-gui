//! `opencode export` files (`{"info": Session, "messages": [{"info", "parts"}]}`): import feeds
//! them through the [`Mapper`] as the server would stream them; export reads the shapes back,
//! with `x-oc` and `<ext>` text restoring every native field.

use super::ids;
use super::json::{self, int_at, put, str_at};
use super::mapper::{HARNESS, Mapper, RootInfo, ext, xattr};
use crate::kit::{NativeCodec, NativeSession};
use loa_contracts::harness::{AdapterError, Result};
use loa_core::{CallStatus, Dom, DomWriter, ElementKind as K, MessageKind, Node, NodeId, PatchSink, attr};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use std::path::Path;

#[derive(Debug, Deserialize)]
struct WithParts {
    info: Json,
    #[serde(default)]
    parts: Vec<Json>,
}

#[derive(Debug, Deserialize)]
struct ExportFile {
    info: Json,
    messages: Vec<WithParts>,
}

/// The OpenCode [`NativeCodec`]. Sessions live in OpenCode's own storage, so none are discovered:
/// the user picks the file `opencode export <id>` wrote. The app session id is the native id.
pub struct OpenCodeNative;

impl NativeCodec for OpenCodeNative {
    fn import(path: &Path, sink: &dyn PatchSink) -> Result<()> {
        let file: ExportFile = serde_json::from_str(&std::fs::read_to_string(path)?).map_err(|e| AdapterError::Format(e.to_string()))?;
        let id = str_at(&file.info, "/id").ok_or_else(|| AdapterError::Format("session has no id".into()))?.to_owned();
        let created = int_at(&file.info, "/time/created").ok_or_else(|| AdapterError::Format("session has no time.created".into()))?;
        let w = DomWriter::new(sink);
        let mut mapper = Mapper::start(&w.at(created), RootInfo { session_id: id, native: file.info, ..Default::default() })?;
        let mut last = created;
        for message in &file.messages {
            let at = int_at(&message.info, "/time/created").unwrap_or(last);
            mapper.message_updated(&w.at(at), &message.info)?;
            for part in &message.parts {
                let part_at = int_at(part, "/time/start").or_else(|| int_at(part, "/state/time/start")).unwrap_or(at);
                mapper.part_updated(&w.at(part_at), part)?;
            }
            last = int_at(&message.info, "/time/completed").unwrap_or(at).max(last);
        }
        mapper.finish(&w.at(last))
    }

    fn export(dom: &Dom, out: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(&export_value(dom)?).map_err(|e| AdapterError::Format(e.to_string()))?;
        Ok(std::fs::write(out, text)?)
    }

    fn discover(_home: &Path) -> Vec<NativeSession> {
        Vec::new()
    }
}

fn oc(node: &Node) -> Result<Json> {
    json::parse_object(node.str_attr(xattr::OC).unwrap_or_default()).map_err(|e| AdapterError::Format(format!("x-oc of node {}: {e}", node.id)))
}

fn object(text: &str, node: &Node) -> Result<Json> {
    json::parse_object(text).map_err(|e| AdapterError::Format(format!("ext {}: {e}", node.id)))
}

fn fill(v: &mut Json, fields: impl IntoIterator<Item = (&'static str, Json)>) {
    for (ptr, value) in fields {
        put(v, ptr, value);
    }
}

/// `(provider, model)` of `provider/model`; a name without a slash has no provider.
fn split_model(model: &str) -> (&str, &str) {
    model.split_once('/').unwrap_or(("", model))
}

/// The export document for `dom`.
pub fn export_value(dom: &Dom) -> Result<Json> {
    let root = dom.root().ok_or_else(|| AdapterError::Format("the session DOM is empty".into()))?;
    let created = root.int_attr(attr::CREATED).unwrap_or(0);
    let session_id = root.str_attr(attr::NATIVE_ID).map_or_else(|| ids::ascending("ses", created), str::to_owned);
    let (title, cwd) = (root.str_attr(attr::TITLE), root.str_attr(attr::CWD));
    let mut info = oc(root)?;
    put(&mut info, "/id", json!(session_id));
    fill(&mut info, title.map(|t| ("/title", json!(t))).into_iter().chain(cwd.map(|c| ("/directory", json!(c)))));
    put(&mut info, "/time/created", json!(created));
    if root.attr(xattr::OC).is_none() {
        // A session the app ran on another harness: fill what OpenCode requires.
        fill(&mut info, [("/slug", json!(session_id)), ("/projectID", json!("global")), ("/version", json!("loa")), ("/time/updated", json!(created))]);
        fill(&mut info, [("/title", title.is_none()), ("/directory", cwd.is_none())].into_iter().filter(|(_, missing)| *missing).map(|(ptr, _)| (ptr, json!(""))));
    }
    let ctx = Ctx { session_id: &session_id, model: root.str_attr(attr::MODEL).unwrap_or_default(), cwd: cwd.unwrap_or_default() };
    let mut messages = Vec::new();
    for turn in dom.children_of_kind(root.id, K::Turn) {
        ctx.turn(dom, turn, &mut messages)?;
    }
    Ok(json!({ "info": info, "messages": messages }))
}

struct Ctx<'a> {
    session_id: &'a str,
    model: &'a str,
    cwd: &'a str,
}

/// One message of a turn and its parts.
struct Out {
    id: String,
    info: Json,
    parts: Vec<Json>,
}

impl Ctx<'_> {
    fn turn(&self, dom: &Dom, turn: &Node, messages: &mut Vec<Json>) -> Result<()> {
        let started = turn.int_attr(attr::STARTED).unwrap_or(0);
        let user_id = turn.str_attr(attr::ID).map_or_else(|| ids::ascending("msg", started), str::to_owned);
        let mut user = oc(turn)?;
        fill(&mut user, [("/id", json!(user_id)), ("/sessionID", json!(self.session_id)), ("/role", json!("user")), ("/time/created", json!(started))]);
        if turn.attr(xattr::OC).is_none() {
            let (provider, model) = split_model(self.model);
            fill(&mut user, [("/agent", json!("build")), ("/model", json!({ "providerID": provider, "modelID": model }))]);
        }
        let mut out = vec![Out { id: user_id.clone(), info: user, parts: Vec::new() }];
        // Elements the app wrote without a native message go into one assistant message made on first use.
        let mut synthetic = None;
        for node in dom.children(turn.id) {
            let owner = node.str_attr(xattr::MESSAGE);
            let (message, mut part) = match node.kind {
                K::Ext if node.str_attr(attr::TYPE) == Some(ext::MESSAGE) => {
                    let id = owner.ok_or_else(|| AdapterError::Format(format!("message ext {} has no x-message", node.id)))?;
                    let mut info = object(&node.text, node)?;
                    fill(&mut info, [("/id", json!(id)), ("/sessionID", json!(self.session_id))]);
                    out.push(Out { id: id.to_owned(), info, parts: Vec::new() });
                    continue;
                }
                K::Ext if node.str_attr(attr::TYPE) == Some(ext::PART) && node.str_attr(attr::HARNESS) == Some(HARNESS) => {
                    let message = self.owner(&mut out, &mut synthetic, owner, &user_id, started, node.id);
                    let mut part = object(&node.text, node)?;
                    put(&mut part, "/id", json!(node.str_attr(xattr::PART).unwrap_or_default()));
                    (message, part)
                }
                K::Message => {
                    let message = if node.str_attr(attr::ROLE) == Some("user") && owner.is_none() { 0 } else { self.owner(&mut out, &mut synthetic, owner, &user_id, started, node.id) };
                    let thinking = node.str_attr(attr::KIND) == Some(MessageKind::Thinking.as_str());
                    let mut part = oc(node)?;
                    let id = node.str_attr(attr::ID).map_or_else(|| ids::ascending("prt", started), str::to_owned);
                    fill(&mut part, [("/id", json!(id)), ("/type", json!(if thinking { "reasoning" } else { "text" })), ("/text", json!(node.text))]);
                    if thinking && node.attr(xattr::OC).is_none() {
                        put(&mut part, "/time/start", json!(started));
                    }
                    (message, part)
                }
                K::Call => (self.owner(&mut out, &mut synthetic, owner, &user_id, started, node.id), call(dom, node)?),
                _ => continue,
            };
            fill(&mut part, [("/sessionID", json!(self.session_id)), ("/messageID", json!(out[message].id))]);
            out[message].parts.push(part);
        }
        messages.extend(out.into_iter().map(|m| json!({ "info": m.info, "parts": m.parts })));
        Ok(())
    }

    /// Index in `out` of the message `owner` names, else of the synthetic assistant message.
    fn owner(&self, out: &mut Vec<Out>, synthetic: &mut Option<usize>, owner: Option<&str>, user_id: &str, started: i64, node: NodeId) -> usize {
        if let Some(i) = owner.and_then(|o| out.iter().position(|m| m.id == o)).or(*synthetic) {
            return i;
        }
        let (provider, model) = split_model(self.model);
        let id = ids::ascending("msg", started + i64::try_from(node.0).unwrap_or(0));
        let info = json!({
            "id": id, "sessionID": self.session_id, "role": "assistant", "time": { "created": started }, "parentID": user_id,
            "modelID": model, "providerID": provider, "mode": "build", "agent": "build", "path": { "cwd": self.cwd, "root": self.cwd },
            "cost": 0, "tokens": { "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
        });
        out.push(Out { id, info, parts: Vec::new() });
        *synthetic = Some(out.len() - 1);
        out.len() - 1
    }
}

/// The tool part of a `<call>`.
fn call(dom: &Dom, node: &Node) -> Result<Json> {
    let mut part = oc(node)?;
    let started = node.int_attr(attr::STARTED);
    let id = node.str_attr(xattr::PART).map_or_else(|| ids::ascending("prt", started.unwrap_or(0)), str::to_owned);
    let status = match str_at(&part, "/state/status") {
        Some(s) => s.to_owned(),
        None => match node.str_attr(attr::STATUS).and_then(CallStatus::parse) {
            Some(CallStatus::Pending) => "pending",
            Some(CallStatus::Running) => "running",
            Some(CallStatus::Error | CallStatus::Cancelled) => "error",
            _ => "completed",
        }
        .to_owned(),
    };
    let call_id = node.str_attr(attr::CALL_ID).unwrap_or(&id).to_owned();
    fill(&mut part, [("/id", json!(id)), ("/type", json!("tool")), ("/callID", json!(call_id)), ("/tool", json!(node.str_attr(attr::TOOL).unwrap_or(""))), ("/state/status", json!(status))]);
    let text = |kind| dom.children_of_kind(node.id, kind).next().map_or("", |n| n.text.as_str());
    let input = text(K::Input);
    if status == "pending" {
        put(&mut part, "/state/raw", json!(input));
    } else {
        put(&mut part, "/state/input", json::parse_object(input).unwrap_or_else(|_| json!({ "raw": input })));
    }
    if let Some(title) = node.str_attr(attr::TITLE) {
        put(&mut part, "/state/title", json!(title));
    }
    match status.as_str() {
        "completed" => put(&mut part, "/state/output", json!(text(K::Result))),
        "error" => put(&mut part, "/state/error", json!(text(K::Result))),
        _ => {}
    }
    if node.attr(xattr::OC).is_none() {
        let start = started.unwrap_or(0);
        match status.as_str() {
            "pending" => put(&mut part, "/state/input", Json::Object(Map::new())),
            "running" => put(&mut part, "/state/time/start", json!(start)),
            _ => fill(&mut part, [("/state/time/start", json!(start)), ("/state/time/end", json!(node.int_attr(attr::ENDED).unwrap_or(start)))]),
        }
        if status == "completed" {
            if node.str_attr(attr::TITLE).is_none() {
                put(&mut part, "/state/title", json!(""));
            }
            put(&mut part, "/state/metadata", Json::Object(Map::new()));
        }
    }
    Ok(part)
}
