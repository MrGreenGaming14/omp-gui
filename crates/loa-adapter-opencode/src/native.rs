//! Import and export of OpenCode sessions in the format of `opencode export`
//! and `opencode import`: one JSON object
//! `{"info": Session, "messages": [{"info": Message, "parts": [Part]}]}`.
//!
//! Import feeds the objects through the [`EventMapper`] exactly as the server
//! would stream them, so imported and live sessions have the same shape.
//! Export reads the shapes back. Every native field the schema has no
//! attribute for travels in `x-oc` or `<ext>` text, so exporting an imported
//! session gives back an equal JSON document.

use crate::ids;
use crate::json;
use crate::mapper::{EventMapper, HARNESS, RootInfo, ext, xattr};
use loa_contracts::harness::{AdapterError, Result};
use loa_core::{CallStatus, Dom, ElementKind as K, MessageKind, Node, NodeId, PatchSink, attr};
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use std::path::Path;

/// One message of an export file.
#[derive(Debug, Deserialize)]
pub struct WithParts {
    pub info: Json,
    #[serde(default)]
    pub parts: Vec<Json>,
}

#[derive(Debug, Deserialize)]
struct ExportFile {
    info: Json,
    messages: Vec<WithParts>,
}

/// Reads the export file at `path` into `sink` (an empty DOM). The app
/// session id is the native id; the owner node is left empty for the app to
/// fill when it adopts the session.
pub fn import(path: &Path, sink: &dyn PatchSink) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let file: ExportFile = serde_json::from_str(&text).map_err(|e| AdapterError::Format(e.to_string()))?;
    import_value(file.info, &file.messages, sink)
}

/// Imports an already parsed session and its messages.
pub fn import_value(info: Json, messages: &[WithParts], sink: &dyn PatchSink) -> Result<()> {
    let id = json::str_at(&info, "/id").ok_or_else(|| AdapterError::Format("session has no id".into()))?.to_owned();
    let created = json::int_at(&info, "/time/created").ok_or_else(|| AdapterError::Format("session has no time.created".into()))?;
    let root = RootInfo { session_id: id, native: info, ..Default::default() };
    let mut mapper = EventMapper::start(sink, root, created)?;
    let mut last = created;
    for message in messages {
        let at = json::int_at(&message.info, "/time/created").unwrap_or(last);
        mapper.message_updated(sink, &message.info, at)?;
        for part in &message.parts {
            let part_at = json::int_at(part, "/time/start")
                .or_else(|| json::int_at(part, "/state/time/start"))
                .unwrap_or(at);
            mapper.part_updated(sink, part, part_at)?;
        }
        last = json::int_at(&message.info, "/time/completed").unwrap_or(at).max(last);
    }
    mapper.finish(sink, last)
}

/// Writes `dom` as an export file at `out`.
pub fn export(dom: &Dom, out: &Path) -> Result<()> {
    let value = export_value(dom)?;
    let text = serde_json::to_string_pretty(&value).map_err(|e| AdapterError::Format(e.to_string()))?;
    std::fs::write(out, text)?;
    Ok(())
}

fn oc(node: &Node) -> Result<Json> {
    let text = node.str_attr(xattr::OC).unwrap_or_default();
    json::parse_object(text).map_err(|e| AdapterError::Format(format!("x-oc of node {}: {e}", node.id)))
}

fn str_or<'a>(node: &'a Node, name: &str, default: &'a str) -> &'a str {
    node.str_attr(name).unwrap_or(default)
}

/// Splits `provider/model`. A name without a slash has no provider.
fn split_model(model: &str) -> (&str, &str) {
    model.split_once('/').unwrap_or(("", model))
}

/// Builds the export document for `dom`.
pub fn export_value(dom: &Dom) -> Result<Json> {
    let root = dom.root().ok_or_else(|| AdapterError::Format("the session DOM is empty".into()))?;
    let created = root.int_attr(attr::CREATED).unwrap_or(0);
    let session_id = root
        .str_attr(attr::NATIVE_ID)
        .map_or_else(|| ids::ascending("ses", created), str::to_owned);
    let mut info = oc(root)?;
    let native = root.attr(xattr::OC).is_some();
    json::put(&mut info, "/id", json!(session_id));
    if let Some(title) = root.str_attr(attr::TITLE) {
        json::put(&mut info, "/title", json!(title));
    }
    if let Some(cwd) = root.str_attr(attr::CWD) {
        json::put(&mut info, "/directory", json!(cwd));
    }
    json::put(&mut info, "/time/created", json!(created));
    if !native {
        // A session the app ran on another harness: fill what OpenCode requires.
        json::put(&mut info, "/slug", json!(session_id));
        json::put(&mut info, "/projectID", json!("global"));
        json::put(&mut info, "/version", json!("loa"));
        json::put(&mut info, "/time/updated", json!(created));
        if root.str_attr(attr::TITLE).is_none() {
            json::put(&mut info, "/title", json!(""));
        }
        if root.str_attr(attr::CWD).is_none() {
            json::put(&mut info, "/directory", json!(""));
        }
    }
    let model = root.str_attr(attr::MODEL).unwrap_or_default();
    let mut ctx = Ctx { session_id: &session_id, model, cwd: root.str_attr(attr::CWD).unwrap_or_default() };
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

/// Messages of one turn, in order, with their parts.
struct Out {
    id: String,
    info: Json,
    parts: Vec<Json>,
}

impl Ctx<'_> {
    fn turn(&mut self, dom: &Dom, turn: &Node, messages: &mut Vec<Json>) -> Result<()> {
        let started = turn.int_attr(attr::STARTED).unwrap_or(0);
        let user_id = turn.str_attr(attr::ID).map_or_else(|| ids::ascending("msg", started), str::to_owned);
        let mut user = oc(turn)?;
        let native = turn.attr(xattr::OC).is_some();
        json::put(&mut user, "/id", json!(user_id));
        json::put(&mut user, "/sessionID", json!(self.session_id));
        json::put(&mut user, "/role", json!("user"));
        json::put(&mut user, "/time/created", json!(started));
        if !native {
            let (provider, model) = split_model(self.model);
            json::put(&mut user, "/agent", json!("build"));
            json::put(&mut user, "/model", json!({ "providerID": provider, "modelID": model }));
        }
        let mut out: Vec<Out> = vec![Out { id: user_id.clone(), info: user, parts: Vec::new() }];
        // Elements the app wrote without a native message go into one
        // assistant message created on first use.
        let mut synthetic: Option<usize> = None;
        for node in dom.children(turn.id) {
            let owner = node.str_attr(xattr::MESSAGE);
            match node.kind {
                K::Ext if node.str_attr(attr::TYPE) == Some(ext::MESSAGE) => {
                    let id = owner.ok_or_else(|| AdapterError::Format(format!("message ext {} has no x-message", node.id)))?;
                    let mut info = json::parse_object(&node.text)
                        .map_err(|e| AdapterError::Format(format!("message ext {}: {e}", node.id)))?;
                    json::put(&mut info, "/id", json!(id));
                    json::put(&mut info, "/sessionID", json!(self.session_id));
                    out.push(Out { id: id.to_owned(), info, parts: Vec::new() });
                }
                K::Ext if node.str_attr(attr::TYPE) == Some(ext::PART) && node.str_attr(attr::HARNESS) == Some(HARNESS) => {
                    let id = str_or(node, xattr::PART, "");
                    let mut part = json::parse_object(&node.text)
                        .map_err(|e| AdapterError::Format(format!("part ext {}: {e}", node.id)))?;
                    let message = self.owner(&mut out, &mut synthetic, owner, &user_id, started, node.id);
                    json::put(&mut part, "/id", json!(id));
                    json::put(&mut part, "/sessionID", json!(self.session_id));
                    json::put(&mut part, "/messageID", json!(out[message].id));
                    out[message].parts.push(part);
                }
                K::Message => {
                    let user_text = node.str_attr(attr::ROLE) == Some("user");
                    let message = if user_text && owner.is_none() {
                        0
                    } else {
                        self.owner(&mut out, &mut synthetic, owner, &user_id, started, node.id)
                    };
                    let mut part = oc(node)?;
                    let at = started;
                    let id = node.str_attr(attr::ID).map_or_else(|| ids::ascending("prt", at), str::to_owned);
                    let thinking = node.str_attr(attr::KIND) == Some(MessageKind::Thinking.as_str());
                    json::put(&mut part, "/id", json!(id));
                    json::put(&mut part, "/sessionID", json!(self.session_id));
                    json::put(&mut part, "/messageID", json!(out[message].id));
                    json::put(&mut part, "/type", json!(if thinking { "reasoning" } else { "text" }));
                    json::put(&mut part, "/text", json!(node.text));
                    if thinking && node.attr(xattr::OC).is_none() {
                        json::put(&mut part, "/time/start", json!(at));
                    }
                    out[message].parts.push(part);
                }
                K::Call => {
                    let message = self.owner(&mut out, &mut synthetic, owner, &user_id, started, node.id);
                    let part = self.call(dom, node, &out[message].id)?;
                    out[message].parts.push(part);
                }
                _ => {}
            }
        }
        messages.extend(out.into_iter().map(|m| json!({ "info": m.info, "parts": m.parts })));
        Ok(())
    }

    /// Index in `out` of the message `owner` names, or of the synthetic
    /// assistant message for elements with no native message.
    fn owner(
        &self,
        out: &mut Vec<Out>,
        synthetic: &mut Option<usize>,
        owner: Option<&str>,
        user_id: &str,
        started: i64,
        node: NodeId,
    ) -> usize {
        if let Some(i) = owner.and_then(|o| out.iter().position(|m| m.id == o)) {
            return i;
        }
        if let Some(i) = *synthetic {
            return i;
        }
        let (provider, model) = split_model(self.model);
        let id = ids::ascending("msg", started + i64::try_from(node.0).unwrap_or(0));
        let info = json!({
            "id": id,
            "sessionID": self.session_id,
            "role": "assistant",
            "time": { "created": started },
            "parentID": user_id,
            "modelID": model,
            "providerID": provider,
            "mode": "build",
            "agent": "build",
            "path": { "cwd": self.cwd, "root": self.cwd },
            "cost": 0,
            "tokens": { "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } },
        });
        out.push(Out { id, info, parts: Vec::new() });
        *synthetic = Some(out.len() - 1);
        out.len() - 1
    }

    fn call(&self, dom: &Dom, node: &Node, message_id: &str) -> Result<Json> {
        let mut part = oc(node)?;
        let started = node.int_attr(attr::STARTED);
        let id = node.str_attr(xattr::PART).map_or_else(|| ids::ascending("prt", started.unwrap_or(0)), str::to_owned);
        json::put(&mut part, "/id", json!(id));
        json::put(&mut part, "/sessionID", json!(self.session_id));
        json::put(&mut part, "/messageID", json!(message_id));
        json::put(&mut part, "/type", json!("tool"));
        json::put(&mut part, "/callID", json!(str_or(node, attr::CALL_ID, &id)));
        json::put(&mut part, "/tool", json!(str_or(node, attr::TOOL, "")));
        let status = match json::str_at(&part, "/state/status") {
            Some(s) => s.to_owned(),
            None => match node.str_attr(attr::STATUS).and_then(CallStatus::parse) {
                Some(CallStatus::Pending) => "pending",
                Some(CallStatus::Running) => "running",
                Some(CallStatus::Error | CallStatus::Cancelled) => "error",
                _ => "completed",
            }
            .to_owned(),
        };
        let native = node.attr(xattr::OC).is_some();
        json::put(&mut part, "/state/status", json!(status));
        let input = dom.children_of_kind(node.id, K::Input).next().map_or("", |n| n.text.as_str());
        let result = dom.children_of_kind(node.id, K::Result).next().map_or("", |n| n.text.as_str());
        if status == "pending" {
            json::put(&mut part, "/state/raw", json!(input));
        } else {
            let args = json::parse_object(input).unwrap_or_else(|_| json!({ "raw": input }));
            json::put(&mut part, "/state/input", args);
        }
        if let Some(title) = node.str_attr(attr::TITLE) {
            json::put(&mut part, "/state/title", json!(title));
        }
        match status.as_str() {
            "completed" => json::put(&mut part, "/state/output", json!(result)),
            "error" => json::put(&mut part, "/state/error", json!(result)),
            _ => {}
        }
        if !native {
            if status == "pending" {
                json::put(&mut part, "/state/input", Json::Object(Map::new()));
            } else {
                let start = started.unwrap_or(0);
                json::put(&mut part, "/state/time/start", json!(start));
                if status != "running" {
                    json::put(&mut part, "/state/time/end", json!(node.int_attr(attr::ENDED).unwrap_or(start)));
                }
                if status == "completed" {
                    if node.str_attr(attr::TITLE).is_none() {
                        json::put(&mut part, "/state/title", json!(""));
                    }
                    json::put(&mut part, "/state/metadata", Json::Object(Map::new()));
                }
            }
        }
        Ok(part)
    }
}
