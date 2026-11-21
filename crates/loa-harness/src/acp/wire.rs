//! The subset of the Agent Client Protocol schema (protocol version 1) the
//! client side reads. Fields the adapter doesn't look at stay in the raw JSON
//! that lands in `<ext>` elements.

use serde::Deserialize;
use serde_json::Value;

/// The only ACP major version this adapter speaks.
pub const PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentCapabilities {
    pub load_session: bool,
    pub mcp_capabilities: McpCapabilities,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct McpCapabilities {
    pub http: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u16,
    #[serde(default)]
    pub agent_capabilities: AgentCapabilities,
    #[serde(default)]
    pub agent_info: Option<Implementation>,
}

#[derive(Debug, Deserialize)]
pub struct Implementation {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: String,
    #[serde(default)]
    pub modes: Option<SessionModeState>,
    /// Unstable `session/set_model` support: the models the agent offers.
    #[serde(default)]
    pub models: Option<SessionModelState>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct LoadSessionResponse {
    pub modes: Option<SessionModeState>,
    pub models: Option<SessionModelState>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModelState {
    pub current_model_id: String,
    #[serde(default)]
    pub available_models: Vec<ModelInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub model_id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionModeState {
    pub current_mode_id: String,
    #[serde(default)]
    pub available_modes: Vec<SessionModeInfo>,
}

#[derive(Debug, Deserialize)]
pub struct SessionModeInfo {
    pub id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: StopReason,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::EndTurn => "end_turn",
            StopReason::MaxTokens => "max_tokens",
            StopReason::MaxTurnRequests => "max_turn_requests",
            StopReason::Refusal => "refusal",
            StopReason::Cancelled => "cancelled",
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Fields of both `tool_call` and `tool_call_update`. A `tool_call` carries
/// at least `title`; an update carries only what changed.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallFields {
    pub tool_call_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub status: Option<ToolCallStatus>,
    /// Replaces the whole content collection when present.
    #[serde(default)]
    pub content: Option<Vec<Value>>,
    #[serde(default)]
    pub raw_input: Option<Value>,
    #[serde(default)]
    pub raw_output: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub entries: Vec<PlanEntry>,
}

#[derive(Debug, Deserialize)]
pub struct PlanEntry {
    pub content: String,
    #[serde(default)]
    pub priority: Option<String>,
    pub status: PlanEntryStatus,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentModeUpdate {
    pub current_mode_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SessionInfoUpdate {
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestPermissionRequest {
    pub tool_call: ToolCallFields,
    pub options: Vec<PermissionOption>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionOptionKind,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

#[derive(Debug, Deserialize)]
pub struct ReadTextFileRequest {
    pub path: String,
    /// 1-based first line.
    #[serde(default)]
    pub line: Option<u32>,
    /// Number of lines.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct WriteTextFileRequest {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalRequest {
    pub command: String,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<Vec<EnvVariable>>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub output_byte_limit: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct EnvVariable {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRef {
    pub terminal_id: String,
}
