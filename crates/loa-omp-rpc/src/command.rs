//! Commands the host writes to omp's stdin, and the typed results some of
//! them return. Field names follow `rpc.md` (camelCase on the wire).

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

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum QueueKind {
    Steering,
    FollowUp,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueueMode {
    #[serde(rename = "all")]
    All,
    #[serde(rename = "one-at-a-time")]
    OneAtATime,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InterruptMode {
    Immediate,
    Wait,
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
#[serde(rename_all = "lowercase")]
pub enum CacheWarmingMode {
    Off,
    Streaming,
    Idle,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalOp {
    Get,
    Create,
    Resume,
    Pause,
    Drop,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoPhase {
    pub name: String,
    #[serde(default)]
    pub tasks: Vec<TodoItem>,
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

/// A URL scheme the host serves; omp calls it back with `host_uri_request`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostUriScheme {
    pub scheme: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub immutable: Option<bool>,
}

/// Every command of `rpc.md`. The client adds the `id`.
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
    Steer {
        message: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageContent>,
    },
    FollowUp {
        message: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageContent>,
    },
    RemoveQueuedMessage {
        message: String,
        queue: QueueKind,
    },
    PromoteQueuedMessage {
        message: String,
    },
    Abort,
    AbortAndPrompt {
        message: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageContent>,
    },
    AbortAndRestoreQueue,
    NewSession {
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_session: Option<String>,
    },
    OpenSession {
        session_dir: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model_id: Option<String>,
    },
    NegotiateProtocol {
        protocol_version: u32,
    },
    GetState,
    SetFastMode {
        enabled: bool,
    },
    SetSlowMode {
        enabled: bool,
    },
    Goal {
        op: GoalOp,
        #[serde(skip_serializing_if = "Option::is_none")]
        objective: Option<String>,
        #[serde(rename = "token_budget", skip_serializing_if = "Option::is_none")]
        token_budget: Option<u64>,
    },
    SetAskDialog {
        enabled: bool,
    },
    GetAvailableCommands,
    GetEntries {
        #[serde(skip_serializing_if = "Option::is_none")]
        since: Option<String>,
    },
    GetTree,
    SetTodos {
        phases: Vec<TodoPhase>,
    },
    SetHostTools {
        tools: Vec<HostToolDefinition>,
    },
    SetHostUriSchemes {
        schemes: Vec<HostUriScheme>,
    },
    SetSubagentSubscription {
        level: SubagentLevel,
    },
    SetEventFilter {
        /// `None` forwards every event type.
        events: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message_updates: Option<MessageUpdates>,
    },
    GetSubagents,
    GetSubagentMessages {
        #[serde(skip_serializing_if = "Option::is_none")]
        subagent_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_file: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_byte: Option<u64>,
    },
    CancelSubagent {
        subagent_id: String,
    },
    SteerSubagent {
        subagent_id: String,
        message: String,
    },
    LiveStart {
        #[serde(skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    LiveConnect {
        sdp_offer: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    LiveStop,
    LiveMute {
        #[serde(skip_serializing_if = "Option::is_none")]
        muted: Option<bool>,
    },
    DictationStart {
        capture_id: String,
    },
    DictationStop,
    SetModel {
        provider: String,
        model_id: String,
    },
    CycleModel,
    GetAvailableModels,
    SetThinkingLevel {
        /// `inherit`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh` or `max`.
        level: String,
    },
    CycleThinkingLevel,
    GetAvailableThinkingLevels,
    SetSteeringMode {
        mode: QueueMode,
    },
    SetFollowUpMode {
        mode: QueueMode,
    },
    SetInterruptMode {
        mode: InterruptMode,
    },
    Compact {
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    SetAutoCompaction {
        enabled: bool,
    },
    SetCacheWarming {
        mode: CacheWarmingMode,
    },
    SetAutoRetry {
        enabled: bool,
    },
    AbortRetry,
    Bash {
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        exclude_from_context: Option<bool>,
    },
    AbortBash,
    GetSessionStats,
    GetSessionLease {
        session_ids: Vec<String>,
    },
    ExportHtml {
        #[serde(skip_serializing_if = "Option::is_none")]
        output_path: Option<String>,
    },
    SwitchSession {
        session_path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        model_id: Option<String>,
    },
    Branch {
        entry_id: String,
    },
    Fork {
        #[serde(skip_serializing_if = "Option::is_none")]
        entry_id: Option<String>,
    },
    NavigateTree {
        entry_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        summarize: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    AbortBranchSummary,
    SetLabel {
        entry_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    GetBranchMessages,
    GetLastAssistantText,
    SetSessionName {
        name: String,
    },
    Handoff {
        #[serde(skip_serializing_if = "Option::is_none")]
        custom_instructions: Option<String>,
    },
    GetMessages,
    GetMessagesPage {
        #[serde(skip_serializing_if = "Option::is_none")]
        cursor: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
    },
    GetLoginProviders,
    Login {
        provider_id: String,
    },
    GetLogoutAccounts {
        provider_id: String,
    },
    Logout {
        provider_id: String,
        credential_id: i64,
    },
    PredictWord {
        text: String,
        cursor: u32,
    },
    PredictWordFeedback {
        text: String,
        cursor: u32,
        suggestion: String,
        accepted: bool,
    },
    Btw {
        question: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        record_id: Option<String>,
    },
    BtwCancel {
        #[serde(skip_serializing_if = "Option::is_none")]
        record_id: Option<String>,
    },
    GetBtwHistory,
}

/// The `ready` frame omp writes before anything else.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ready {
    /// Version of the ready frame itself; absent on old servers (v1).
    #[serde(default)]
    pub protocol_version: Option<u32>,
    #[serde(default)]
    pub supported_protocol_versions: Option<Vec<u32>>,
    #[serde(default)]
    pub max_frame_bytes: Option<u64>,
    #[serde(default)]
    pub max_reassembled_frame_bytes: Option<u64>,
    #[serde(default)]
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
    #[serde(default)]
    pub agent_invoked: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub api: String,
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Everything else omp reports (cost, thinking config, headers, …).
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ModelInfo {
    /// `provider/id`, the selector form omp uses everywhere.
    pub fn selector(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct LoginProvider {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub authenticated: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    pub summary: String,
    #[serde(default)]
    pub short_summary: Option<String>,
    #[serde(default)]
    pub first_kept_entry_id: String,
    #[serde(default)]
    pub tokens_before: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub tokens: i64,
    pub context_window: i64,
    pub percent: f64,
}

/// `data` of `get_state`. Fields the client doesn't name stay in `extra`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionState {
    #[serde(default)]
    pub model: Option<ModelInfo>,
    #[serde(default)]
    pub thinking_level: Option<String>,
    #[serde(default)]
    pub is_streaming: bool,
    #[serde(default)]
    pub is_compacting: bool,
    #[serde(default)]
    pub session_file: Option<String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub session_name: Option<String>,
    #[serde(default)]
    pub message_count: u64,
    #[serde(default)]
    pub is_settled: bool,
    #[serde(default)]
    pub todo_phases: Vec<TodoPhase>,
    #[serde(default)]
    pub context_usage: Option<ContextUsage>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `data` of `abort_and_restore_queue`: the user input omp withdrew from its
/// queues before aborting, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoredQueue {
    #[serde(default)]
    pub steering: Vec<QueuedInput>,
    #[serde(default)]
    pub follow_up: Vec<QueuedInput>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct QueuedInput {
    /// The queue-chip text.
    pub text: String,
    #[serde(default)]
    pub images: Vec<ImageContent>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct CancellationResult {
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSessionResult {
    pub cancelled: bool,
    pub resumed: bool,
    pub session_id: String,
    #[serde(default)]
    pub session_file: Option<String>,
}
