//! Everything this crate knows about omp2's wire formats, in one place.
//!
//! omp2 is the Rust rewrite of omp on the `omp2` branch of
//! <https://github.com/can1357/oh-my-pi> (the README links the future home,
//! `stencil-hq/omp`, which is not public yet). Shapes below were read from
//! commit [`SOURCE_COMMIT`] and are marked:
//!
//! - **verified**: copied from omp2 source, file named on the item
//! - **assumed**: not in omp2 yet; the shape this crate expects until omp2
//!   publishes its own (see `docs/omp2-tracking.md`)
//!
//! When omp2 changes, this module is the only place to update.

use serde::de::{self, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value as Json, json};
use std::fmt;

/// omp2 commit the verified shapes were read from (2026-09-04).
pub const SOURCE_COMMIT: &str = "2f92f3b5aa2035d21c9f016f9ff6350795e5685a";

/// Stable, nonzero id of an omp2 DOM node, minted monotonically and never
/// reused. Verified: `crates/dom/src/arena.rs`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Deserialize)]
#[serde(transparent)]
pub struct Handle(pub u64);

/// The fixed `<session><meta/><body/><queues/></session>` skeleton every
/// omp2 DOM starts with, handles 1 to 4. Verified: `crates/dom/src/txn.rs`
/// (`Dom::new`).
pub const ROOT: Handle = Handle(1);
pub const META: Handle = Handle(2);
pub const BODY: Handle = Handle(3);
pub const QUEUES: Handle = Handle(4);

/// Closed structural element names. Any other name is a tool or custom tag.
/// Verified: `crates/vocab/src/lib.rs` (`KnownTag`, kebab-case).
pub const KNOWN_TAGS: &[&str] = &[
    "session", "meta", "body", "queues", "todo", "item", "jobs", "job", "directors", "director", "con", "var",
    "compaction", "turn", "user", "assistant", "developer", "notice", "steering", "prompts", "prompt", "subagent",
    "input", "result", "diag", "usage",
];

/// Custom tag of one provider content block under `<assistant>`, carrying
/// `kind` (`text` or `thinking`), the custom prop [`BLOCK_INDEX_PROP`] and a
/// streamed `text` prop. Verified: `crates/session/src/projection.rs`,
/// `crates/agent/src/loop.rs`.
pub const ASSISTANT_CONTENT_TAG: &str = "assistant-content";
/// Verified: `crates/session/src/projection.rs` (`PROVIDER_BLOCK_INDEX_PROP`).
pub const BLOCK_INDEX_PROP: &str = "index";

/// An element name. On the wire a custom name carries a `tool:` prefix; a
/// bare name that is not in [`KNOWN_TAGS`] is custom too. Verified:
/// `crates/vocab/src/lib.rs` (`Tag::from_str`, `Serialize for Tag`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub custom: bool,
}

impl Tag {
    pub fn parse(wire: &str) -> Self {
        match wire.strip_prefix("tool:") {
            Some(name) => Tag { name: name.to_owned(), custom: true },
            None => Tag { name: wire.to_owned(), custom: !KNOWN_TAGS.contains(&wire) },
        }
    }

    /// True for the structural element `name`.
    pub fn is(&self, name: &str) -> bool {
        !self.custom && self.name == name
    }
}

impl<'de> Deserialize<'de> for Tag {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        Ok(Tag::parse(&wire))
    }
}

/// A property name with the wire `custom:` prefix removed. Well-known names
/// are kebab-case (`tokens-in`, `stop-reason`). Verified:
/// `crates/vocab/src/lib.rs` (`PropKey`, `PropId`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PropName(pub String);

impl PropName {
    pub fn parse(wire: &str) -> Self {
        PropName(wire.strip_prefix("custom:").unwrap_or(wire).to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for PropName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = std::borrow::Cow::<'de, str>::deserialize(deserializer)?;
        Ok(PropName::parse(&wire))
    }
}

/// A property value. Arrays and objects stay as their JSON text. Verified:
/// `crates/dom/src/node.rs` (`Value`, untagged).
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Json(String),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Json::deserialize(deserializer)? {
            Json::Null => Value::Null,
            Json::Bool(v) => Value::Bool(v),
            Json::Number(n) => match n.as_i64() {
                Some(v) => Value::Int(v),
                // omp2 stores integers above i64::MAX as floats too.
                None => Value::Float(n.as_f64().unwrap_or(f64::NAN)),
            },
            Json::String(v) => Value::Str(v),
            other => Value::Json(other.to_string()),
        })
    }
}

/// Data of a node to insert; omp2 mints the handle. Props are an ordered list
/// of `[name, value]` pairs. Verified: `crates/dom/src/node.rs` (`NodeSpec`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct NodeSpec {
    pub tag: Tag,
    #[serde(default)]
    pub props: Vec<(PropName, Value)>,
    #[serde(default)]
    pub content: Option<String>,
}

/// One operation of a `patch@1` transaction. On the wire each is an array:
/// `["ins", parent, after, node]`, `["rm", h]`, `["set", h, prop, value]`,
/// `["mv", h, parent, after]`. `after: null` means the first position.
/// Verified: `crates/dom/src/op.rs`.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Ins { parent: Handle, after: Option<Handle>, node: NodeSpec },
    Rm(Handle),
    Set { h: Handle, prop: PropName, value: Value },
    Mv { h: Handle, parent: Handle, after: Option<Handle> },
}

impl<'de> Deserialize<'de> for Op {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(OpVisitor)
    }
}

struct OpVisitor;

fn next<'de, A: SeqAccess<'de>, T: Deserialize<'de>>(seq: &mut A, index: usize) -> Result<T, A::Error> {
    seq.next_element()?.ok_or_else(|| de::Error::invalid_length(index, &OpVisitor))
}

impl<'de> Visitor<'de> for OpVisitor {
    type Value = Op;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an omp2 DOM operation array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Op, A::Error> {
        let code: std::borrow::Cow<'de, str> = next(&mut seq, 0)?;
        let op = match &*code {
            "ins" => Op::Ins { parent: next(&mut seq, 1)?, after: next(&mut seq, 2)?, node: next(&mut seq, 3)? },
            "rm" => Op::Rm(next(&mut seq, 1)?),
            "set" => Op::Set { h: next(&mut seq, 1)?, prop: next(&mut seq, 2)?, value: next(&mut seq, 3)? },
            "mv" => Op::Mv { h: next(&mut seq, 1)?, parent: next(&mut seq, 2)?, after: next(&mut seq, 3)? },
            other => return Err(de::Error::unknown_variant(other, &["ins", "rm", "set", "mv"])),
        };
        if seq.next_element::<IgnoredAny>()?.is_some() {
            return Err(de::Error::custom("DOM operation array has trailing elements"));
        }
        Ok(op)
    }
}

/// One committed `patch@1` transaction: `{cause, prior, label, ops}`.
/// `cause` and `prior` are journal entry ids this crate does not need.
/// Verified: `crates/dom/src/subscribe.rs` (`Patch`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Patch {
    #[serde(default)]
    pub label: Option<String>,
    pub ops: Vec<Op>,
}

/// Operation of an append-only text stream bound to one node property.
/// Verified: `crates/dom/src/op.rs` (`StreamOp`, lowercase).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamOp {
    Open,
    Append,
    Close,
}

/// One node of a canonical snapshot. Verified: `crates/dom/src/snapshot.rs`
/// (`CanonicalNode`) and `crates/dom/src/node.rs` (`Node`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SnapshotNode {
    pub handle: Handle,
    pub parent: Option<Handle>,
    pub node: SnapshotBody,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SnapshotBody {
    pub tag: Tag,
    #[serde(default)]
    pub props: Vec<(PropName, Value)>,
    #[serde(default)]
    pub kids: Vec<Handle>,
    #[serde(default)]
    pub content: Option<String>,
}

/// A stream still open when the snapshot was taken; `text` is everything
/// appended so far. Verified: `crates/dom/src/stream.rs` (`SnapshotStream`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct SnapshotStream {
    pub sid: u32,
    pub node: Handle,
    pub prop: PropName,
    pub text: String,
}

/// A whole omp2 DOM. Verified: `crates/dom/src/snapshot.rs`
/// (`EncodedSnapshot`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Snapshot {
    pub high_water: u64,
    #[serde(default)]
    pub nodes: Vec<SnapshotNode>,
    #[serde(default)]
    pub streams: Vec<SnapshotStream>,
}

/// One replicated DOM event, as one JSON object. Verified as the record
/// shape omp2's collab host replicates to native guests:
/// `crates/driver/src/collab/session.rs` (`event_record`, `ReplicatedEvent`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind")]
pub enum DomEvent {
    #[serde(rename = "patch@1")]
    Patch { data: Patch },
    #[serde(rename = "stream@1")]
    Stream {
        sid: u32,
        op: StreamOp,
        #[serde(default)]
        node: Option<Handle>,
        #[serde(default)]
        prop: Option<PropName>,
        #[serde(default)]
        text: Option<String>,
    },
    /// Replaces the replica: sent first on subscription and after branch
    /// navigation.
    #[serde(rename = "snapshot@1")]
    Snapshot { data: Snapshot },
}

// ---------------------------------------------------------------------------
// Process transport. `omp --mode rpc` speaks protocol v1: one JSON object per
// line on stdin and stdout. Verified: `crates/app/src/rpc_mode.rs`,
// `crates/rpc/src/protocol.rs`.
// ---------------------------------------------------------------------------

/// Arguments that put the omp2 binary (`omp`) in RPC mode. Verified:
/// `crates/app/Cargo.toml` (`[[bin]] name = "omp"`), `crates/app/src/cli.rs`.
pub const RPC_ARGS: &[&str] = &["--mode", "rpc"];
/// Flag that reopens a session by id. Verified: `crates/app/src/cli.rs`.
pub const RESUME_FLAG: &str = "--resume";
/// Model as `provider/model` and thinking level. Verified:
/// `crates/app/src/cli.rs` (`ChatArgs`).
pub const MODEL_FLAG: &str = "--model";
pub const THINKING_FLAG: &str = "--thinking";

/// `set_model {provider, modelId}`. Verified: `rpc_mode.rs`.
pub fn set_model(id: &str, provider: &str, model: &str) -> String {
    request(id, "set_model", params(json!({ "provider": provider, "modelId": model })))
}

/// `set_thinking_level {level}`. Verified: `rpc_mode.rs`.
pub fn set_thinking(id: &str, level: &str) -> String {
    request(id, "set_thinking_level", params(json!({ "level": level })))
}

/// Levels `set_thinking_level` takes, lowest first. Verified: `rpc_mode.rs`
/// (`cycle_thinking_level`).
pub const THINKING_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh"];

/// One entry of the `get_available_models` response `{models: [...]}`.
/// Verified: `rpc_mode.rs` (`rpc_models`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider: String,
    /// The model has a thinking setting.
    #[serde(default)]
    pub reasoning: bool,
}

/// The `models` of a `get_available_models` response.
pub fn available_models(data: &Json) -> Result<Vec<ModelEntry>, serde_json::Error> {
    Vec::<ModelEntry>::deserialize(data.get("models").unwrap_or(&Json::Null))
}

/// **Assumed.** omp2's RPC stream deliberately carries projected
/// `message_*`/`tool_execution_*` events and not `patch@1`
/// (`crates/app/tests/rpc_spine.rs`: "private journal patches are not part of
/// the public RPC event stream"). This crate asks for the DOM with this
/// command, named after the existing `set_subagent_subscription`, and expects
/// [`DomEvent`] records as stdout lines afterwards, starting with a snapshot.
/// An omp2 without it answers with an error response, which the adapter
/// reports as a version mismatch.
pub const DOM_SUBSCRIPTION: &str = "set_dom_subscription";

/// A request line: `{"id", "type", ...params}` with params flattened.
/// Verified: `crates/rpc/src/protocol.rs` (`RpcRequest`).
pub fn request(id: &str, command: &str, params: Map<String, Json>) -> String {
    let mut object = params;
    object.insert("id".into(), Json::String(id.to_owned()));
    object.insert("type".into(), Json::String(command.to_owned()));
    let mut line = Json::Object(object).to_string();
    line.push('\n');
    line
}

fn params(value: Json) -> Map<String, Json> {
    match value {
        Json::Object(map) => map,
        _ => Map::new(),
    }
}

/// **Assumed** parameters of [`DOM_SUBSCRIPTION`].
pub fn subscribe_dom(id: &str) -> String {
    request(id, DOM_SUBSCRIPTION, params(json!({ "enabled": true })))
}

/// `prompt {message, images?}`; images are `{data: base64, mimeType}`.
/// Verified: `rpc_mode.rs` (`plain_request_input`).
pub fn prompt(id: &str, text: &str, images: Vec<(String, String)>) -> String {
    let mut map = params(json!({ "message": text }));
    if !images.is_empty() {
        let images = images.into_iter().map(|(data, mime)| json!({ "data": data, "mimeType": mime })).collect();
        map.insert("images".into(), Json::Array(images));
    }
    request(id, "prompt", map)
}

/// `abort`, `get_state`, `quit` take no parameters. Verified: `rpc_mode.rs`.
pub fn bare(id: &str, command: &str) -> String {
    request(id, command, Map::new())
}

/// `compact {customInstructions?}`. Verified: `rpc_mode.rs`.
pub fn compact(id: &str, instructions: Option<&str>) -> String {
    let mut map = Map::new();
    if let Some(text) = instructions {
        map.insert("customInstructions".into(), Json::String(text.to_owned()));
    }
    request(id, "compact", map)
}

/// `approve {promptId, approved, scope, reason?}`; scope is `once`, `call`,
/// `turn`, `session` or `persist`. Verified: `rpc_mode.rs`
/// (`approve_response`), `crates/agent/src/approvals.rs` (`ApprovalScope`).
pub fn approve(id: &str, prompt_id: &str, approved: bool, scope: &str, reason: Option<&str>) -> String {
    let mut map = params(json!({ "promptId": prompt_id, "approved": approved, "scope": scope }));
    if let Some(reason) = reason {
        map.insert("reason".into(), Json::String(reason.to_owned()));
    }
    request(id, "approve", map)
}

/// `extension_ui_response` answering a `select` request: `id` is the UI
/// request id, `values` the picked labels, `customInput` typed text.
/// Verified: `rpc_mode.rs` (`selected_values`, `RpcUiBridge::respond`).
pub fn select_response(ui_id: &str, values: &[String], custom: Option<&str>) -> String {
    let mut map = params(json!({ "values": values }));
    if let Some(text) = custom {
        map.insert("customInput".into(), Json::String(text.to_owned()));
    }
    request(ui_id, "extension_ui_response", map)
}

/// A frame read from omp2's stdout that is not a [`DomEvent`].
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// First line: `{"type":"ready", "protocolVersion", ...}`.
    Ready,
    /// `{"type":"response", id, command, success, data?, error?, code?}`.
    Response { id: String, success: bool, data: Json, error: Option<String>, code: Option<String> },
    /// `{"type":"agent_end", cancelled, error?}`: the turn is over.
    AgentEnd { cancelled: bool, error: Option<String> },
    /// `{"type":"tool_approval_request", promptId, toolCallId, title, body,
    /// kind, scopes, timeoutMs}`. Verified: `rpc_mode.rs` (`kernel_event_value`).
    Approval { prompt_id: String, call_id: Option<String>, title: Option<String>, body: Option<String>, timeout_ms: Option<i64> },
    /// `{"type":"extension_ui_request", id, method:"select", title, options,
    /// multi}`. Verified: `rpc_mode.rs` (`RpcUiBridge::present`).
    Select { id: String, title: Option<String>, options: Vec<String>, multi: bool },
    /// `{"type":"session_shutdown"}`.
    Shutdown,
    /// Projections of DOM changes this crate reads from the DOM instead.
    Other,
}

/// What one stdout line holds.
#[derive(Debug)]
pub enum Line {
    Dom(DomEvent),
    Frame(Frame),
}

#[derive(Deserialize)]
struct Probe<'a> {
    #[serde(borrow, default)]
    kind: Option<std::borrow::Cow<'a, str>>,
}

/// Parses one stdout line. DOM records carry `kind`, RPC frames `type`.
pub fn parse_line(line: &str) -> Result<Line, serde_json::Error> {
    let probe: Probe<'_> = serde_json::from_str(line)?;
    if probe.kind.is_some() {
        return serde_json::from_str(line).map(Line::Dom);
    }
    let value: Json = serde_json::from_str(line)?;
    Ok(Line::Frame(frame(value)))
}

fn text(value: &Json, key: &str) -> Option<String> {
    value.get(key).and_then(Json::as_str).map(str::to_owned)
}

/// Request ids may be strings or numbers. Verified: `protocol.rs` (`RequestId`).
fn id(value: &Json) -> Option<String> {
    match value.get("id")? {
        Json::String(s) => Some(s.clone()),
        Json::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn frame(value: Json) -> Frame {
    match value.get("type").and_then(Json::as_str).unwrap_or_default() {
        "ready" => Frame::Ready,
        "response" => Frame::Response {
            id: id(&value).unwrap_or_default(),
            success: value.get("success").and_then(Json::as_bool).unwrap_or(false),
            error: text(&value, "error"),
            code: text(&value, "code"),
            data: value.get("data").cloned().unwrap_or(Json::Null),
        },
        "agent_end" => Frame::AgentEnd {
            cancelled: value.get("cancelled").and_then(Json::as_bool).unwrap_or(false),
            error: text(&value, "error"),
        },
        "tool_approval_request" => match text(&value, "promptId") {
            Some(prompt_id) => Frame::Approval {
                prompt_id,
                call_id: text(&value, "toolCallId"),
                title: text(&value, "title"),
                body: text(&value, "body"),
                timeout_ms: value.get("timeoutMs").and_then(Json::as_i64),
            },
            None => Frame::Other,
        },
        "extension_ui_request" if value.get("method").and_then(Json::as_str) == Some("select") => match id(&value) {
            Some(id) => Frame::Select {
                id,
                title: text(&value, "title"),
                options: value
                    .get("options")
                    .and_then(Json::as_array)
                    .map(|list| list.iter().filter_map(Json::as_str).map(str::to_owned).collect())
                    .unwrap_or_default(),
                multi: value.get("multi").and_then(Json::as_bool).unwrap_or(false),
            },
            None => Frame::Other,
        },
        "session_shutdown" => Frame::Shutdown,
        _ => Frame::Other,
    }
}

/// Error code of a response refused because a turn is running. Verified:
/// `rpc_mode.rs` (`busy_response`, `RpcErrorCode::SESSION_BUSY`).
pub const SESSION_BUSY: &str = "session_busy";

/// Session id in a `get_state` response. Verified: `rpc_mode.rs` (`rpc_state`).
pub fn session_id(data: &Json) -> Option<&str> {
    data.get("sessionId").and_then(Json::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_adr_0003_patch_example() {
        // docs/adr/0003-one-authoritative-session-tree.md on the omp2 branch.
        let line = r#"{"kind":"patch@1","data":{"cause":"01ARZ3NDEKTSV4RRFFQ69G5FAV","prior":null,"label":"todo.done","ops":[["set",412,"status","completed"],["set",415,"status","in_progress"]]}}"#;
        let Line::Dom(DomEvent::Patch { data }) = parse_line(line).unwrap() else { panic!("not a patch") };
        assert_eq!(data.label.as_deref(), Some("todo.done"));
        assert_eq!(
            data.ops[0],
            Op::Set { h: Handle(412), prop: PropName("status".into()), value: Value::Str("completed".into()) }
        );
    }

    #[test]
    fn decodes_insert_with_custom_tag_and_props() {
        let line = r#"{"kind":"patch@1","data":{"cause":"x","ops":[["ins",9,null,{"tag":"tool:read","props":[["id","c1"],["custom:execution-started",true],["args",{"path":"a"}]],"content":"{}"}],["mv",10,3,9],["rm",11]]}}"#;
        let Line::Dom(DomEvent::Patch { data }) = parse_line(line).unwrap() else { panic!("not a patch") };
        let Op::Ins { parent, after, node } = &data.ops[0] else { panic!("not an insert") };
        assert_eq!((*parent, *after), (Handle(9), None));
        assert_eq!(node.tag, Tag { name: "read".into(), custom: true });
        assert_eq!(node.props[1].0, PropName("execution-started".into()));
        assert_eq!(node.props[2].1, Value::Json(r#"{"path":"a"}"#.into()));
        assert_eq!(data.ops[1], Op::Mv { h: Handle(10), parent: Handle(3), after: Some(Handle(9)) });
        assert_eq!(data.ops[2], Op::Rm(Handle(11)));
    }

    #[test]
    fn rejects_an_operation_with_trailing_elements() {
        let line = r#"{"kind":"patch@1","data":{"ops":[["rm",5,6]]}}"#;
        assert!(parse_line(line).is_err());
    }

    #[test]
    fn tells_dom_records_from_rpc_frames() {
        let stream = r#"{"kind":"stream@1","cause":"x","sid":3,"op":"append","node":null,"prop":null,"text":"hé"}"#;
        assert!(matches!(
            &parse_line(stream).unwrap(),
            Line::Dom(DomEvent::Stream { sid: 3, op: StreamOp::Append, text: Some(t), .. }) if t == "hé"
        ));
        let end = r#"{"type":"agent_end","cancelled":true,"text":"x"}"#;
        assert!(matches!(parse_line(end).unwrap(), Line::Frame(Frame::AgentEnd { cancelled: true, error: None })));
        let response = r#"{"type":"response","id":7,"command":"prompt","success":false,"error":"busy","code":"session_busy"}"#;
        let Line::Frame(Frame::Response { id, code, .. }) = parse_line(response).unwrap() else { panic!() };
        assert_eq!((id.as_str(), code.as_deref()), ("7", Some(SESSION_BUSY)));
    }

    #[test]
    fn decodes_available_models_as_rpc_models_writes_them() {
        let data = json!({ "models": [
            { "id": "claude-haiku-5-5", "name": "Claude Haiku 5.5", "provider": "anthropic", "contextWindow": 200000, "reasoning": true },
            { "id": "plain", "name": "Plain", "provider": "local", "reasoning": false },
        ] });
        let models = available_models(&data).unwrap();
        assert_eq!(models[0], ModelEntry { id: "claude-haiku-5-5".into(), name: "Claude Haiku 5.5".into(), provider: "anthropic".into(), reasoning: true });
        assert!(!models[1].reasoning);
        assert!(available_models(&json!({})).is_err());
        let line: Json = serde_json::from_str(&set_thinking("9", "high")).unwrap();
        assert_eq!(line, json!({ "id": "9", "type": "set_thinking_level", "level": "high" }));
    }
}
