//! Frames omp writes to stdout: responses, session events, UI requests and
//! host callbacks.

use crate::command::{CompactionResult, ModelInfo, Ready, TodoItem};
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;

/// One logical stdout frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    Ready(Ready),
    Response(Response),
    Event(Event),
    Ui(UiRequest),
    HostToolCall(HostToolCall),
    HostToolCancel { id: String, target_id: String },
    HostUriRequest(HostUriRequest),
    HostUriCancel { id: String, target_id: String },
    /// One piece of an oversized frame (protocol v2).
    Chunk(Chunk),
}

/// The answer to a command.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Response {
    #[serde(default)]
    pub id: Option<String>,
    pub command: String,
    pub success: bool,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chunk {
    pub chunk_id: String,
    pub index: u32,
    pub count: u32,
    pub byte_length: u64,
    pub data: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostToolCall {
    pub id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UriOperation {
    Read,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct HostUriRequest {
    pub id: String,
    pub operation: UriOperation,
    pub url: String,
    /// The replacement text of a write.
    #[serde(default)]
    pub content: Option<String>,
}

/// The `role` of a message, without parsing the rest of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct MessageRole {
    #[serde(default)]
    pub role: String,
}

/// A streaming step of an assistant message (`assistantMessageEvent`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum AssistantEvent {
    Start,
    TextStart { content_index: u32 },
    TextDelta { content_index: u32, delta: String },
    TextEnd { content_index: u32, content: String },
    ThinkingStart { content_index: u32 },
    ThinkingDelta { content_index: u32, delta: String },
    ThinkingEnd { content_index: u32, content: String },
    ImageEnd { content_index: u32 },
    ToolcallStart {
        content_index: u32,
        /// The tool call shell (id, name) in `delta` projection mode.
        #[serde(default)]
        block: Option<Value>,
        /// The accumulated message in `full` mode, which also carries the shell.
        #[serde(default)]
        partial: Option<Value>,
    },
    ToolcallDelta { content_index: u32, delta: String },
    ToolcallEnd { content_index: u32, tool_call: Value },
    Done { reason: String },
    Error { reason: String },
    #[serde(other)]
    Other,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptStatus {
    Completed,
    Aborted,
    Error,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptError {
    pub message: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentStatus {
    Started,
    Completed,
    Failed,
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentLifecycle {
    pub id: String,
    pub agent: String,
    #[serde(default)]
    pub description: Option<String>,
    pub status: SubagentStatus,
    #[serde(default)]
    pub session_file: Option<String>,
    #[serde(default)]
    pub parent_tool_call_id: Option<String>,
    #[serde(default)]
    pub index: i64,
    #[serde(default)]
    pub detached: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentProgress {
    #[serde(default)]
    pub index: i64,
    pub agent: String,
    #[serde(default)]
    pub task: String,
    #[serde(default)]
    pub parent_tool_call_id: Option<String>,
    #[serde(default)]
    pub progress: Value,
    #[serde(default)]
    pub session_file: Option<String>,
}

/// A session event or notification. Types the client doesn't model stay
/// available in `Unknown` with their raw JSON.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    AgentStart,
    AgentEnd {
        messages: Vec<Value>,
        is_terminal: Option<bool>,
        yielded: Option<bool>,
        awaiting_async_work: bool,
    },
    TurnStart,
    TurnEnd,
    MessageStart { message: Value, message_id: Option<String> },
    MessageUpdate { role: String, event: AssistantEvent, message_id: Option<String> },
    MessageEnd { message: Value, message_id: Option<String> },
    ToolExecutionStart { tool_call_id: String, tool_name: String, args: Option<Value>, intent: Option<String> },
    ToolExecutionUpdate { tool_call_id: String, tool_name: String, args: Option<Value>, partial_result: Option<Value> },
    ToolExecutionEnd { tool_call_id: String, tool_name: String, result: Option<Value>, is_error: bool },
    CompactionStart { auto: bool, reason: Option<String> },
    CompactionEnd { auto: bool, result: Option<CompactionResult>, aborted: bool, skipped: bool, error_message: Option<String> },
    ModelChanged,
    ThinkingLevelChanged { thinking_level: Option<String> },
    TodoReminder { todos: Vec<TodoItem> },
    TodoAutoClear,
    Notice { level: String, message: String, source: Option<String> },
    PromptResult { id: Option<String>, agent_invoked: bool, status: PromptStatus, error: Option<PromptError>, session_settled: bool },
    SessionSettled,
    SubagentLifecycle(SubagentLifecycle),
    SubagentProgress(SubagentProgress),
    ConfigUpdate { model: Option<ModelInfo>, thinking_level: Option<String> },
    SessionInfoUpdate { title: Option<String>, session_id: String },
    /// Any other frame. `kind` is its `type`.
    Unknown { kind: String, raw: Value },
}

impl Event {
    /// The `type` string of the frame this event came from.
    pub fn kind(&self) -> &str {
        match self {
            Event::AgentStart => "agent_start",
            Event::AgentEnd { .. } => "agent_end",
            Event::TurnStart => "turn_start",
            Event::TurnEnd => "turn_end",
            Event::MessageStart { .. } => "message_start",
            Event::MessageUpdate { .. } => "message_update",
            Event::MessageEnd { .. } => "message_end",
            Event::ToolExecutionStart { .. } => "tool_execution_start",
            Event::ToolExecutionUpdate { .. } => "tool_execution_update",
            Event::ToolExecutionEnd { .. } => "tool_execution_end",
            Event::CompactionStart { auto: true, .. } => "auto_compaction_start",
            Event::CompactionStart { auto: false, .. } => "manual_compaction_start",
            Event::CompactionEnd { auto: true, .. } => "auto_compaction_end",
            Event::CompactionEnd { auto: false, .. } => "manual_compaction_end",
            Event::ModelChanged => "model_changed",
            Event::ThinkingLevelChanged { .. } => "thinking_level_changed",
            Event::TodoReminder { .. } => "todo_reminder",
            Event::TodoAutoClear => "todo_auto_clear",
            Event::Notice { .. } => "notice",
            Event::PromptResult { .. } => "prompt_result",
            Event::SessionSettled => "session_settled",
            Event::SubagentLifecycle(_) => "subagent_lifecycle",
            Event::SubagentProgress(_) => "subagent_progress",
            Event::ConfigUpdate { .. } => "config_update",
            Event::SessionInfoUpdate { .. } => "session_info_update",
            Event::Unknown { kind, .. } => kind,
        }
    }
}

/// One question of an `ask` UI request.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AskQuestion {
    pub id: String,
    pub question: String,
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<AskOption>,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub recommended: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AskOption {
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// An `extension_ui_request`. Dialogs (`select`, `confirm`, `input`,
/// `editor`, `ask`) wait for a [`UiResponse`](crate::UiResponse); the rest
/// are fire and forget.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "method", rename_all_fields = "camelCase")]
pub enum UiRequest {
    #[serde(rename = "select")]
    Select {
        id: String,
        title: String,
        options: Vec<String>,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        selection_marker: Option<String>,
        #[serde(default)]
        checked_indices: Vec<u32>,
        #[serde(default)]
        initial_index: Option<u32>,
    },
    #[serde(rename = "confirm")]
    Confirm {
        id: String,
        title: String,
        #[serde(default)]
        message: String,
        #[serde(default)]
        timeout: Option<u64>,
    },
    #[serde(rename = "input")]
    Input {
        id: String,
        title: String,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        timeout: Option<u64>,
        #[serde(default)]
        secret: bool,
    },
    #[serde(rename = "editor")]
    Editor {
        id: String,
        title: String,
        #[serde(default)]
        prefill: Option<String>,
    },
    #[serde(rename = "ask")]
    Ask {
        id: String,
        questions: Vec<AskQuestion>,
        #[serde(default)]
        timeout: Option<u64>,
    },
    /// omp closed the dialog `target_id` itself (timeout or abort).
    #[serde(rename = "cancel")]
    Cancel { id: String, target_id: String },
    #[serde(rename = "notify")]
    Notify {
        id: String,
        message: String,
        #[serde(default)]
        notify_type: Option<String>,
    },
    #[serde(rename = "setStatus")]
    SetStatus {
        id: String,
        status_key: String,
        #[serde(default)]
        status_text: Option<String>,
    },
    #[serde(rename = "setWidget")]
    SetWidget {
        id: String,
        widget_key: String,
        #[serde(default)]
        widget_lines: Option<Vec<String>>,
    },
    #[serde(rename = "setTitle")]
    SetTitle { id: String, title: String },
    #[serde(rename = "set_editor_text")]
    SetEditorText { id: String, text: String },
    #[serde(rename = "open_url")]
    OpenUrl {
        id: String,
        url: String,
        #[serde(default)]
        launch_url: Option<String>,
        #[serde(default)]
        instructions: Option<String>,
    },
}

impl UiRequest {
    pub fn id(&self) -> &str {
        match self {
            UiRequest::Select { id, .. }
            | UiRequest::Confirm { id, .. }
            | UiRequest::Input { id, .. }
            | UiRequest::Editor { id, .. }
            | UiRequest::Ask { id, .. }
            | UiRequest::Cancel { id, .. }
            | UiRequest::Notify { id, .. }
            | UiRequest::SetStatus { id, .. }
            | UiRequest::SetWidget { id, .. }
            | UiRequest::SetTitle { id, .. }
            | UiRequest::SetEditorText { id, .. }
            | UiRequest::OpenUrl { id, .. } => id,
        }
    }

    /// Whether omp waits for an answer.
    pub fn is_dialog(&self) -> bool {
        matches!(
            self,
            UiRequest::Select { .. }
                | UiRequest::Confirm { .. }
                | UiRequest::Input { .. }
                | UiRequest::Editor { .. }
                | UiRequest::Ask { .. }
        )
    }
}

#[derive(Deserialize)]
struct Head<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentEndWire {
    #[serde(default)]
    messages: Vec<Value>,
    #[serde(default)]
    is_terminal: Option<bool>,
    #[serde(default)]
    yielded: Option<bool>,
    #[serde(default)]
    awaiting_async_work: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageWire {
    #[serde(default)]
    message: Value,
    #[serde(default)]
    message_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MessageUpdateWire {
    #[serde(default)]
    message: MessageRole,
    assistant_message_event: AssistantEvent,
    #[serde(default)]
    message_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolWire {
    tool_call_id: String,
    tool_name: String,
    #[serde(default)]
    args: Option<Value>,
    #[serde(default)]
    intent: Option<String>,
    #[serde(default)]
    partial_result: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    is_error: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompactionWire {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    result: Option<CompactionResult>,
    #[serde(default)]
    aborted: bool,
    #[serde(default)]
    skipped: bool,
    #[serde(default)]
    error_message: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PromptResultWire {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    agent_invoked: bool,
    status: PromptStatus,
    #[serde(default)]
    error: Option<PromptError>,
    #[serde(default)]
    session_settled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MiscWire {
    #[serde(default)]
    thinking_level: Option<String>,
    #[serde(default)]
    todos: Vec<TodoItem>,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    model: Option<ModelInfo>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelWire {
    id: String,
    target_id: String,
}

#[derive(Deserialize)]
struct PayloadWire<T> {
    payload: T,
}

impl Frame {
    /// Parses one stdout line. A frame whose `type` is known but whose body
    /// doesn't match the expected shape becomes [`Event::Unknown`], so a newer
    /// omp never stops the client.
    pub fn parse(line: &[u8]) -> Result<Frame, serde_json::Error> {
        let head: Head<'_> = serde_json::from_slice(line)?;
        let kind = head.kind.unwrap_or_default();
        match parse_known(&kind, line) {
            Ok(Some(frame)) => Ok(frame),
            Ok(None) | Err(_) => {
                let raw: Value = serde_json::from_slice(line)?;
                Ok(Frame::Event(Event::Unknown { kind: kind.into_owned(), raw }))
            }
        }
    }
}

fn parse_known(kind: &str, line: &[u8]) -> Result<Option<Frame>, serde_json::Error> {
    use serde_json::from_slice as from;
    let event = |e: Event| -> Result<Option<Frame>, serde_json::Error> { Ok(Some(Frame::Event(e))) };
    match kind {
        "ready" => Ok(Some(Frame::Ready(from(line)?))),
        "response" => Ok(Some(Frame::Response(from(line)?))),
        "rpc_chunk" => Ok(Some(Frame::Chunk(from(line)?))),
        "extension_ui_request" => Ok(Some(Frame::Ui(from(line)?))),
        "host_tool_call" => Ok(Some(Frame::HostToolCall(from(line)?))),
        "host_uri_request" => Ok(Some(Frame::HostUriRequest(from(line)?))),
        "host_tool_cancel" => {
            let c: CancelWire = from(line)?;
            Ok(Some(Frame::HostToolCancel { id: c.id, target_id: c.target_id }))
        }
        "host_uri_cancel" => {
            let c: CancelWire = from(line)?;
            Ok(Some(Frame::HostUriCancel { id: c.id, target_id: c.target_id }))
        }
        "message_update" => {
            let w: MessageUpdateWire = from(line)?;
            event(Event::MessageUpdate { role: w.message.role, event: w.assistant_message_event, message_id: w.message_id })
        }
        "message_start" | "message_end" => {
            let w: MessageWire = from(line)?;
            if kind == "message_start" {
                event(Event::MessageStart { message: w.message, message_id: w.message_id })
            } else {
                event(Event::MessageEnd { message: w.message, message_id: w.message_id })
            }
        }
        "tool_execution_start" | "tool_execution_update" | "tool_execution_end" => {
            let w: ToolWire = from(line)?;
            event(match kind {
                "tool_execution_start" => Event::ToolExecutionStart {
                    tool_call_id: w.tool_call_id,
                    tool_name: w.tool_name,
                    args: w.args,
                    intent: w.intent,
                },
                "tool_execution_update" => Event::ToolExecutionUpdate {
                    tool_call_id: w.tool_call_id,
                    tool_name: w.tool_name,
                    args: w.args,
                    partial_result: w.partial_result,
                },
                _ => Event::ToolExecutionEnd {
                    tool_call_id: w.tool_call_id,
                    tool_name: w.tool_name,
                    result: w.result,
                    is_error: w.is_error,
                },
            })
        }
        "agent_start" => event(Event::AgentStart),
        "agent_end" => {
            let w: AgentEndWire = from(line)?;
            event(Event::AgentEnd {
                messages: w.messages,
                is_terminal: w.is_terminal,
                yielded: w.yielded,
                awaiting_async_work: w.awaiting_async_work,
            })
        }
        "turn_start" => event(Event::TurnStart),
        "turn_end" => event(Event::TurnEnd),
        "auto_compaction_start" | "manual_compaction_start" | "auto_compaction_end" | "manual_compaction_end" => {
            let w: CompactionWire = from(line)?;
            let auto = kind.starts_with("auto");
            if kind.ends_with("_start") {
                event(Event::CompactionStart { auto, reason: w.reason })
            } else {
                event(Event::CompactionEnd {
                    auto,
                    result: w.result,
                    aborted: w.aborted,
                    skipped: w.skipped,
                    error_message: w.error_message,
                })
            }
        }
        "prompt_result" => {
            let w: PromptResultWire = from(line)?;
            event(Event::PromptResult {
                id: w.id,
                agent_invoked: w.agent_invoked,
                status: w.status,
                error: w.error,
                session_settled: w.session_settled,
            })
        }
        "session_settled" => event(Event::SessionSettled),
        "model_changed" => event(Event::ModelChanged),
        "todo_auto_clear" => event(Event::TodoAutoClear),
        "subagent_lifecycle" => {
            let w: PayloadWire<SubagentLifecycle> = from(line)?;
            event(Event::SubagentLifecycle(w.payload))
        }
        "subagent_progress" => {
            let w: PayloadWire<SubagentProgress> = from(line)?;
            event(Event::SubagentProgress(w.payload))
        }
        "thinking_level_changed" | "todo_reminder" | "notice" | "config_update" | "session_info_update" => {
            let w: MiscWire = from(line)?;
            event(match kind {
                "thinking_level_changed" => Event::ThinkingLevelChanged { thinking_level: w.thinking_level },
                "todo_reminder" => Event::TodoReminder { todos: w.todos },
                "notice" => Event::Notice {
                    level: w.level.unwrap_or_default(),
                    message: w.message.unwrap_or_default(),
                    source: w.source,
                },
                "config_update" => Event::ConfigUpdate { model: w.model, thinking_level: w.thinking_level },
                _ => Event::SessionInfoUpdate { title: w.title, session_id: w.session_id.unwrap_or_default() },
            })
        }
        _ => Ok(None),
    }
}
