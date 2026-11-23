//! Commands the host writes to omp's stdin, and the typed results some of
//! them return. Field names follow `rpc.md` (camelCase on the wire). Only
//! the commands the app sends are modelled.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// An image attached to a prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "image", rename_all = "camelCase")]
pub struct ImageContent {
    /// Base64 encoded bytes.
    pub data: String,
    pub mime_type: String,
}

/// How a prompt sent during a running turn is queued.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}

/// Which subagent frames omp forwards.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentLevel {
    Off,
    Progress,
    Events,
}

/// `set_event_filter` projection of `message_update` frames.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageUpdates {
    /// Every update carries the accumulated message.
    Full,
    /// Updates carry only the delta, which keeps token streaming cheap.
    Delta,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

/// A tool the host serves; omp calls it back with `host_tool_call`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostToolDefinition {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub description: String,
    /// JSON Schema of the arguments.
    pub parameters: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
    /// `essential` or `discoverable`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load_mode: Option<String>,
}

/// The commands of `rpc.md` the app uses. The client adds the `id`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Command {
    Prompt {
        message: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageContent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        streaming_behavior: Option<StreamingBehavior>,
    },
    Steer { message: String },
    Abort,
    NewSession,
    /// Withdraws queued user input and aborts, atomically.
    AbortAndRestoreQueue,
    NegotiateProtocol { protocol_version: u32 },
    GetState,
    GetEntries,
    SetAskDialog { enabled: bool },
    SetHostTools { tools: Vec<HostToolDefinition> },
    SetSubagentSubscription { level: SubagentLevel },
    SetEventFilter {
        /// `None` forwards every event type.
        events: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message_updates: Option<MessageUpdates>,
    },
    SetModel { provider: String, model_id: String },
    GetAvailableModels,
    /// `inherit`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh` or `max`.
    SetThinkingLevel { level: String },
    Compact {
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    Fork {
        #[serde(skip_serializing_if = "Option::is_none")]
        entry_id: Option<String>,
    },
    GetLoginProviders,
    Login { provider_id: String },
    GetLogoutAccounts { provider_id: String },
    Logout { provider_id: String, credential_id: i64 },
}

/// The `ready` frame omp writes before anything else.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Ready {
    /// Version of the ready frame itself; absent on old servers (v1).
    pub protocol_version: Option<u32>,
    pub supported_protocol_versions: Option<Vec<u32>>,
    pub supports_bash_exclude_from_context: bool,
}

impl Ready {
    /// Protocol versions omp accepts. Servers that predate negotiation speak v1 only.
    pub fn offered_versions(&self) -> Vec<u32> {
        match &self.supported_protocol_versions {
            Some(versions) if !versions.is_empty() => versions.clone(),
            _ => vec![self.protocol_version.unwrap_or(1)],
        }
    }
}

/// `data` of a `prompt` response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptAck {
    /// `Some(false)`: the prompt finished locally and no `prompt_result` follows.
    pub agent_invoked: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub provider: String,
    #[serde(default)]
    pub reasoning: bool,
    /// Everything else omp reports (api, cost, thinking config, …).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ModelInfo {
    /// `provider/id`, the selector form omp uses everywhere.
    pub fn selector(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// `data` of `get_available_models`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ModelList {
    #[serde(default)]
    pub models: Vec<ModelInfo>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    pub summary: String,
    pub tokens_before: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `data` of `get_state`. Fields the client doesn't name stay in `extra`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    pub model: Option<ModelInfo>,
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub is_streaming: bool,
    pub session_file: Option<String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct CancellationResult {
    pub cancelled: bool,
}
