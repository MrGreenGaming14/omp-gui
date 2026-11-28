//! omp2's wire shapes (oh-my-pi `omp2` branch, commit 2f92f3b5): `patch@1` DOM records and the RPC v1 frames the
//! mapper reads. `set_dom_subscription` is assumed: omp2's public RPC stream carries no DOM yet.

use serde::{Deserialize, Deserializer};
use serde_json::Value as Json;

/// A stable omp2 node id; 1 to 4 are the `<session><meta/><body/><queues/></session>` skeleton.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
pub struct Handle(pub u64);

pub const ROOT: Handle = Handle(1);
pub const BODY: Handle = Handle(3);

const KNOWN_TAGS: [&str; 26] = [
    "session", "meta", "body", "queues", "todo", "item", "jobs", "job", "directors", "director", "con", "var", "compaction",
    "turn", "user", "assistant", "developer", "notice", "steering", "prompts", "prompt", "subagent", "input", "result", "diag", "usage",
];

/// An element name; custom (tool) names carry a `tool:` prefix on the wire or aren't known.
#[derive(Clone, Debug, Deserialize)]
#[serde(from = "String")]
pub struct Tag {
    pub name: String,
    pub custom: bool,
}

impl From<String> for Tag {
    fn from(wire: String) -> Self {
        match wire.strip_prefix("tool:") {
            Some(name) => Tag { name: name.to_owned(), custom: true },
            None => Tag { custom: !KNOWN_TAGS.contains(&wire.as_str()), name: wire },
        }
    }
}

impl Tag {
    /// True for the structural element `name`.
    pub fn is(&self, name: &str) -> bool {
        !self.custom && self.name == name
    }
}

/// A property name without the wire `custom:` prefix.
#[derive(Debug, Deserialize)]
#[serde(from = "String")]
pub struct Prop(pub String);

impl From<String> for Prop {
    fn from(wire: String) -> Self {
        Prop(wire.strip_prefix("custom:").map_or_else(|| wire.clone(), str::to_owned))
    }
}

/// A node to insert or a snapshot node; props are ordered `[name, value]` pairs.
#[derive(Debug, Deserialize)]
pub struct NodeSpec {
    pub tag: Tag,
    #[serde(default)]
    pub props: Vec<(Prop, Json)>,
    #[serde(default)]
    pub kids: Vec<Handle>,
    #[serde(default)]
    pub content: Option<String>,
}

/// The name leading an [`Op`] array: `ins`, `rm`, `set` or `mv` for `C` 0 to 3.
#[derive(Debug)]
pub struct Code<const C: usize>;

impl<'de, const C: usize> Deserialize<'de> for Code<C> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let code = String::deserialize(d)?;
        if code == ["ins", "rm", "set", "mv"][C] { Ok(Code) } else { Err(serde::de::Error::custom(format!("not `{code}`"))) }
    }
}

/// `["ins", parent, after, node]`, `["rm", h]`, `["set", h, prop, value]` or `["mv", h, parent, after]`; `after: null` is first.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Op {
    Ins(Code<0>, Handle, Option<Handle>, NodeSpec),
    Rm(Code<1>, Handle),
    Set(Code<2>, Handle, Prop, Json),
    Mv(Code<3>, Handle, Handle, Option<Handle>),
}

#[derive(Debug, Deserialize)]
pub struct Patch {
    pub ops: Vec<Op>,
}

#[derive(Copy, Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamOp { Open, Append, Close }

#[derive(Debug, Deserialize)]
pub struct SnapshotNode {
    pub handle: Handle,
    pub parent: Option<Handle>,
    pub node: NodeSpec,
}

/// A stream open at the snapshot; `text` is what it appended so far.
#[derive(Debug, Deserialize)]
pub struct SnapshotStream {
    pub sid: u32,
    pub node: Handle,
    pub prop: Prop,
    pub text: String,
}

#[derive(Debug, Deserialize)]
pub struct Snapshot {
    pub high_water: u64,
    #[serde(default)]
    pub nodes: Vec<SnapshotNode>,
    #[serde(default)]
    pub streams: Vec<SnapshotStream>,
}

/// One replicated DOM record; a snapshot replaces the replica (first on subscription, after branch navigation).
#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
pub enum DomEvent {
    #[serde(rename = "patch@1")]
    Patch { data: Patch },
    #[serde(rename = "stream@1")]
    Stream { sid: u32, op: StreamOp, node: Option<Handle>, prop: Option<Prop>, text: Option<String> },
    #[serde(rename = "snapshot@1")]
    Snapshot { data: Snapshot },
}

/// The RPC frames the mapper reads; DOM projections are `Other`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Frame {
    AgentEnd {
        #[serde(default)]
        cancelled: bool,
        error: Option<String>,
    },
    ToolApprovalRequest {
        prompt_id: String,
        tool_call_id: Option<String>,
        title: Option<String>,
        body: Option<String>,
        #[serde(default)]
        scopes: Option<Vec<String>>,
        timeout_ms: Option<i64>,
    },
    ExtensionUiRequest {
        id: Json,
        method: String,
        title: Option<String>,
        #[serde(default)]
        options: Vec<String>,
        #[serde(default)]
        multi: bool,
    },
    #[serde(other)]
    Other,
}

/// One stdout line: DOM records carry `kind`, RPC frames `type`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Input {
    Dom(DomEvent),
    Frame(Frame),
}

/// A mapper reply: `approve` goes out as a request, `extension_ui_response` as a bare frame.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Reply {
    /// Scope is `once`, `call`, `turn`, `session` or `persist`.
    Approve {
        prompt_id: String,
        approved: bool,
        scope: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Answers a `select` UI request: picked labels plus typed text.
    ExtensionUiResponse {
        id: Json,
        values: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_input: Option<String>,
    },
}

/// One `get_available_models` entry.
#[derive(Clone, Debug, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub reasoning: bool,
}
