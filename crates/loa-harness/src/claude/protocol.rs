//! Claude Code's stream JSON lines, decoded borrowing from the parsed line.
//! Unknown fields are ignored; unknown kinds decode to an `Other` variant.

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::{Value, json};

/// One stdout line, by its `type`.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Line<'a> {
    System(#[serde(borrow)] System<'a>),
    StreamEvent {
        #[serde(default)]
        parent_tool_use_id: Option<&'a str>,
        #[serde(borrow)]
        event: Stream<'a>,
    },
    Assistant(#[serde(borrow)] Message<'a>),
    User(#[serde(borrow)] Message<'a>),
    Result(#[serde(borrow)] TurnResult<'a>),
    ControlRequest {
        #[serde(default)]
        request_id: &'a str,
        #[serde(borrow)]
        request: Option<Request<'a>>,
    },
    ControlCancelRequest {
        #[serde(default)]
        request_id: Option<&'a str>,
    },
    KeepAlive,
    ToolProgress,
    #[serde(other)]
    Other,
}

/// A `system` line; which fields are set depends on `subtype`.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct System<'a> {
    pub subtype: &'a str,
    pub session_id: Option<&'a str>,
    pub model: Option<&'a str>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<&'a str>,
    pub compact_metadata: Option<CompactMetadata>,
    pub task_id: Option<&'a str>,
    pub tool_use_id: Option<&'a str>,
    pub task_type: Option<&'a str>,
    pub description: Option<&'a str>,
    pub status: Option<&'a str>,
    pub summary: Option<&'a str>,
    #[serde(borrow)]
    pub patch: Option<TaskPatch<'a>>,
}

#[derive(Deserialize, Default)]
pub struct CompactMetadata {
    pub pre_tokens: Option<i64>,
}

#[derive(Deserialize, Default)]
pub struct TaskPatch<'a> {
    pub status: Option<&'a str>,
}

/// The Messages API stream event inside a `stream_event` line.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Stream<'a> {
    MessageStart {
        #[serde(borrow)]
        message: StreamMessage<'a>,
    },
    ContentBlockStart {
        index: usize,
        #[serde(borrow)]
        content_block: Block<'a>,
    },
    ContentBlockDelta {
        index: usize,
        #[serde(borrow)]
        delta: StreamDelta<'a>,
    },
    ContentBlockStop {
        index: usize,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct StreamMessage<'a> {
    pub id: &'a str,
    pub model: Option<&'a str>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamDelta<'a> {
    TextDelta {
        #[serde(default)]
        text: &'a str,
    },
    ThinkingDelta {
        #[serde(default)]
        thinking: &'a str,
    },
    InputJsonDelta {
        #[serde(default)]
        partial_json: &'a str,
    },
    #[serde(other)]
    Other,
}

/// An `assistant` or `user` line.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Message<'a> {
    pub parent_tool_use_id: Option<&'a str>,
    pub uuid: Option<&'a str>,
    #[serde(rename = "isReplay")]
    pub is_replay: bool,
    #[serde(rename = "isCompactSummary")]
    pub is_compact_summary: bool,
    #[serde(borrow)]
    pub message: Option<Body<'a>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Body<'a> {
    pub id: &'a str,
    pub model: Option<&'a str>,
    #[serde(borrow)]
    pub content: Option<Content<'a>>,
}

/// A string, a list of content blocks, or anything else (kept as raw JSON).
#[derive(Deserialize)]
#[serde(untagged)]
pub enum Content<'a> {
    Text(&'a str),
    Blocks(#[serde(borrow)] Vec<Block<'a>>),
    Other(IgnoredAny),
}

impl Content<'_> {
    /// The text of a string or of blocks that are all text, joined by lines.
    pub fn text(&self) -> Option<String> {
        match self {
            Content::Text(text) => Some((*text).to_owned()),
            Content::Blocks(blocks) => {
                let mut joined = String::new();
                for block in blocks {
                    let Block::Text { text } = block else { return None };
                    if !joined.is_empty() { joined.push('\n'); }
                    joined.push_str(text);
                }
                Some(joined)
            }
            Content::Other(_) => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block<'a> {
    Text {
        #[serde(default)]
        text: &'a str,
    },
    Thinking {
        #[serde(default)]
        thinking: &'a str,
    },
    #[serde(alias = "server_tool_use", alias = "mcp_tool_use")]
    ToolUse {
        #[serde(default)]
        id: &'a str,
        #[serde(default)]
        name: &'a str,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        #[serde(default)]
        tool_use_id: &'a str,
        #[serde(borrow, default)]
        content: Option<Content<'a>>,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(other)]
    Other,
}

/// The `result` line that ends a turn.
#[derive(Deserialize, Default)]
#[serde(default)]
pub struct TurnResult<'a> {
    pub subtype: Option<&'a str>,
    pub is_error: bool,
    pub terminal_reason: Option<&'a str>,
    pub usage: Option<Usage>,
    pub total_cost_usd: Option<f64>,
    pub duration_ms: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Usage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_input_tokens: Option<i64>,
    pub cache_creation_input_tokens: Option<i64>,
}

/// A control request the CLI sends the host.
#[derive(Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum Request<'a> {
    CanUseTool(#[serde(borrow)] Permission<'a>),
    McpMessage {
        #[serde(default)]
        server_name: &'a str,
        #[serde(default)]
        message: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct Permission<'a> {
    pub tool_name: &'a str,
    pub input: Value,
    pub tool_use_id: Option<&'a str>,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub decision_reason: Option<&'a str>,
    pub blocked_path: Option<&'a str>,
    pub permission_suggestions: Option<Value>,
}

/// A successful `control_response` line.
pub fn control_success(request_id: &str, response: Value) -> Value {
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": response}})
}

/// A failed `control_response` line.
pub fn control_error(request_id: &str, error: &str) -> Value {
    json!({"type": "control_response", "response": {"subtype": "error", "request_id": request_id, "error": error}})
}
