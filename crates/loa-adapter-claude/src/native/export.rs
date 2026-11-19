//! DOM to transcript.
//!
//! Elements made by import are grouped by `x-claude-line` and rebuilt from
//! their envelope, in line order. Elements the app created follow in document
//! order as fresh lines. `<usage>`, `<job>`, `<decision>`, `<todo>`, turns
//! and exts of other harnesses or without a line are derived or app-only and
//! write nothing.

use super::{X_BLOCK, X_CONTENT, X_ENVELOPE, X_LINE, X_PART, X_PATH, X_SPLIT, content, take_field, time};
use crate::HARNESS;
use loa_adapter::{AdapterError, Result};
use loa_core::{Dom, ElementKind, MessageKind, Node, NodeId, Role, Value, attr};
use serde_json::{Map, Value as J, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path};
use uuid::Uuid;

pub(super) fn run(dom: &Dom, out: &Path) -> Result<()> {
    let root = dom.root().ok_or_else(|| AdapterError::Format("the session DOM is empty".to_owned()))?;
    let mut lines: BTreeMap<i64, Vec<(i64, usize, &Node)>> = BTreeMap::new();
    let mut fresh = Vec::new();
    let mut files = Vec::new();
    for (order, node) in dom.iter().enumerate() {
        let line = node.int_attr(X_LINE);
        let part = node.int_attr(X_PART).unwrap_or(-1);
        match node.kind {
            ElementKind::Ext if node.str_attr(attr::HARNESS) == Some(HARNESS) => {
                if let Some(path) = node.str_attr(X_PATH) {
                    let bytes = node.attr(attr::RAW).and_then(Value::as_bytes).unwrap_or(node.text.as_bytes());
                    files.push((path, bytes));
                } else if let Some(line) = line {
                    lines.entry(line).or_default().push((part, order, node));
                }
            }
            ElementKind::Message | ElementKind::Call | ElementKind::Result | ElementKind::Compaction => match line {
                Some(line) => lines.entry(line).or_default().push((part, order, node)),
                None => fresh.push(node),
            },
            _ => {}
        }
    }

    let mut writer = Writer::new(dom, root);
    for mut parts in lines.into_values() {
        parts.sort_by_key(|(part, order, _)| (*part, *order));
        let nodes: Vec<&Node> = parts.into_iter().map(|(_, _, node)| node).collect();
        writer.group(&nodes)?;
    }
    for node in fresh {
        writer.fresh(node)?;
    }

    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, writer.out)?;
    if files.is_empty() {
        return Ok(());
    }
    let dir = super::side_dir(out)
        .ok_or_else(|| AdapterError::Format(format!("{} has no extension to strip for side files", out.display())))?;
    for (path, bytes) in files {
        let rel = Path::new(path);
        if path.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(AdapterError::Format(format!("side file path `{path}` leaves the side directory")));
        }
        let target = dir.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(target, bytes)?;
    }
    Ok(())
}

fn format_error(what: &str, err: serde_json::Error) -> AdapterError {
    AdapterError::Format(format!("{what}: {err}"))
}

/// JSON text of an element, or the text as a JSON string when it doesn't parse.
fn json_or_string(text: &str) -> J {
    serde_json::from_str(text).unwrap_or_else(|_| J::String(text.to_owned()))
}

/// The `x-claude-block` object of an element, or `{"type": default_type}`.
fn leftover(node: &Node, default_type: &str) -> Map<String, J> {
    match node.str_attr(X_BLOCK).and_then(|text| serde_json::from_str(text).ok()) {
        Some(J::Object(block)) => block,
        _ => Map::from_iter([("type".to_owned(), J::from(default_type))]),
    }
}

fn is_thinking(node: &Node) -> bool {
    node.str_attr(attr::KIND) == Some(MessageKind::Thinking.as_str())
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

impl<'d> Writer<'d> {
    fn new(dom: &'d Dom, root: &'d Node) -> Self {
        Self {
            dom,
            root,
            out: String::new(),
            last_uuid: None,
            call_lines: HashMap::new(),
            call_ids: HashMap::new(),
            assistant: None,
        }
    }

    /// Notes what later fresh lines chain to.
    fn track(&mut self, line: &J) {
        if let Some(uuid) = line.get("uuid").and_then(J::as_str)
            && line.get("parentUuid").is_some()
        {
            self.last_uuid = Some(uuid.to_owned());
        }
        if line.get("type").and_then(J::as_str) == Some("user") {
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
        let Some(envelope) = nodes.iter().find_map(|n| n.str_attr(X_ENVELOPE)) else {
            for node in nodes {
                match node.kind {
                    // A whole line kept verbatim.
                    ElementKind::Ext if node.attr(X_PART).is_none() => {
                        if let Ok(line) = serde_json::from_str::<J>(&node.text) {
                            self.track(&line);
                        }
                        self.out.push_str(&node.text);
                        self.out.push('\n');
                    }
                    // Blocks whose envelope element was removed.
                    ElementKind::Ext => {}
                    _ => self.fresh(node)?,
                }
            }
            return Ok(());
        };
        let mut line: Map<String, J> = match serde_json::from_str(envelope) {
            Ok(J::Object(line)) => line,
            Ok(_) => return Err(AdapterError::Format(format!("{X_ENVELOPE} isn't a JSON object"))),
            Err(err) => return Err(format_error(X_ENVELOPE, err)),
        };
        let content = match nodes {
            [node] if node.attr(X_PART).is_none() => self.whole(node),
            _ => Some(J::Array(nodes.iter().map(|n| self.block(n)).collect())),
        };
        if let Some(content) = content {
            let message = line.entry("message").or_insert_with(|| J::Object(Map::new()));
            if let Some(message) = message.as_object_mut() {
                message.insert("content".to_owned(), content);
            }
        }
        if let Some(uuid) = line.get("uuid").and_then(J::as_str) {
            for node in nodes.iter().filter(|n| n.kind == ElementKind::Call) {
                self.call_lines.insert(node.id, uuid.to_owned());
            }
        }
        self.push(J::Object(line));
        Ok(())
    }

    /// `message.content` of a line whose content wasn't an array.
    fn whole(&self, node: &Node) -> Option<J> {
        match node.kind {
            ElementKind::Message => Some(J::String(node.text.clone())),
            ElementKind::Compaction => {
                let items = match take_field(&mut leftover(node, "text"), "content") {
                    Some(J::Array(items)) => items,
                    _ => Vec::new(),
                };
                self.content(node, items)
            }
            _ => Some(self.block(node)),
        }
    }

    /// The content block an element stands for.
    fn block(&self, node: &Node) -> J {
        match node.kind {
            ElementKind::Message => {
                let field = if is_thinking(node) { "thinking" } else { "text" };
                let mut block = leftover(node, field);
                block.insert(field.to_owned(), J::String(node.text.clone()));
                J::Object(block)
            }
            ElementKind::Call => J::Object(self.tool_use(node)),
            ElementKind::Result => J::Object(self.tool_result(node)),
            ElementKind::Ext => json_or_string(&node.text),
            _ => self.whole(node).unwrap_or(J::Null),
        }
    }

    fn content(&self, node: &Node, items: Vec<J>) -> Option<J> {
        let split: Vec<i64> =
            node.attr(X_SPLIT).and_then(Value::as_list).unwrap_or_default().iter().filter_map(Value::as_int).collect();
        let json = node.str_attr(attr::FORMAT) == Some("json");
        content::decode(node.str_attr(X_CONTENT), &node.text, json, &split, items)
    }

    fn call_id(&self, call: NodeId) -> Option<String> {
        let own = self.dom.get(call).and_then(|c| c.str_attr(attr::CALL_ID)).map(str::to_owned);
        own.or_else(|| self.call_ids.get(&call).cloned())
    }

    fn tool_use(&self, node: &Node) -> Map<String, J> {
        let mut block = leftover(node, "tool_use");
        if let Some(id) = self.call_id(node.id) {
            block.insert("id".to_owned(), J::String(id));
        }
        block.insert("name".to_owned(), J::String(node.str_attr(attr::TOOL).unwrap_or_default().to_owned()));
        if let Some(input) = self.dom.children_of_kind(node.id, ElementKind::Input).next() {
            block.insert("input".to_owned(), json_or_string(&input.text));
        } else if node.attr(X_LINE).is_none() {
            block.insert("input".to_owned(), J::Object(Map::new()));
        }
        block
    }

    fn tool_result(&self, node: &Node) -> Map<String, J> {
        let mut block = leftover(node, "tool_result");
        let items = match take_field(&mut block, "content") {
            Some(J::Array(items)) => items,
            _ => Vec::new(),
        };
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

    /// Time for a fresh line: the element's own start, else the nearest
    /// enclosing one. A result uses its call's end.
    fn fresh_ts(&self, node: &Node) -> i64 {
        let ended = (node.kind == ElementKind::Result)
            .then(|| self.dom.get(node.parent)?.int_attr(attr::ENDED))
            .flatten();
        ended
            .or_else(|| {
                std::iter::once(node)
                    .chain(self.dom.ancestors(node.id))
                    .find_map(|n| n.int_attr(attr::STARTED).or_else(|| n.int_attr(attr::CREATED)))
            })
            .unwrap_or(0)
    }

    /// `message.id` for a fresh assistant line: consecutive assistant lines of
    /// one turn share it, like one API response.
    fn assistant_id(&mut self, turn: NodeId) -> String {
        match &self.assistant {
            Some((open, id)) if *open == turn => id.clone(),
            _ => {
                let id = format!("msg_{}", Uuid::new_v4().simple());
                self.assistant = Some((turn, id.clone()));
                id
            }
        }
    }

    fn assistant_message(&mut self, node: &Node, block: Map<String, J>) -> J {
        let mut message = Map::new();
        message.insert("id".to_owned(), J::String(self.assistant_id(node.parent)));
        message.insert("type".to_owned(), J::from("message"));
        message.insert("role".to_owned(), J::from("assistant"));
        if let Some(model) = node.str_attr(attr::MODEL).or_else(|| self.root.str_attr(attr::MODEL)) {
            message.insert("model".to_owned(), J::from(model));
        }
        message.insert("content".to_owned(), J::Array(vec![J::Object(block)]));
        J::Object(message)
    }

    /// Writes an element the app created as a new Claude Code line.
    fn fresh(&mut self, node: &Node) -> Result<()> {
        let uuid = Uuid::new_v4().to_string();
        let mut extra = Map::new();
        let user = |content: J| json!({ "role": "user", "content": content });
        let (ty, message) = match node.kind {
            ElementKind::Message => match node.str_attr(attr::ROLE).and_then(Role::parse) {
                Some(Role::Assistant) => {
                    let J::Object(block) = self.block(node) else { return Ok(()) };
                    ("assistant", self.assistant_message(node, block))
                }
                Some(Role::System) => {
                    extra.insert("isMeta".to_owned(), J::Bool(true));
                    ("user", user(J::String(node.text.clone())))
                }
                _ => ("user", user(J::String(node.text.clone()))),
            },
            ElementKind::Call => {
                if node.str_attr(attr::CALL_ID).is_none() {
                    self.call_ids.insert(node.id, format!("toolu_{}", Uuid::new_v4().simple()));
                }
                self.call_lines.insert(node.id, uuid.clone());
                let block = self.tool_use(node);
                ("assistant", self.assistant_message(node, block))
            }
            ElementKind::Result => {
                if let Some(source) = self.call_lines.get(&node.parent) {
                    extra.insert("sourceToolAssistantUUID".to_owned(), J::String(source.clone()));
                }
                ("user", user(J::Array(vec![J::Object(self.tool_result(node))])))
            }
            ElementKind::Compaction => {
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
        line.insert("timestamp".to_owned(), J::String(time::format(self.fresh_ts(node))));
        line.extend(extra);
        self.push(J::Object(line));
        Ok(())
    }
}
