//! Frames omp writes to stdout: session events, UI requests and host tool calls.

use super::command::{CompactionResult, ModelInfo, TodoItem};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostToolCall {
    pub id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    #[serde(default)]
    pub arguments: Value,
}

/// A streaming step of an assistant message (`assistantMessageEvent`).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum AssistantEvent {
    TextStart { content_index: u32 },
    TextDelta { content_index: u32, delta: String },
    TextEnd { content_index: u32, content: String },
    ThinkingStart { content_index: u32 },
    ThinkingDelta { content_index: u32, delta: String },
    ThinkingEnd { content_index: u32, content: String },
    ToolcallStart {
        content_index: u32,
        /// The tool call shell (id, name) in `delta` projection mode.
        block: Option<Value>,
        /// The accumulated message in `full` mode, which also carries the shell.
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
    pub provider: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub retryable: bool,
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
    pub description: Option<String>,
    pub status: SubagentStatus,
    pub session_file: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentProgress {
    #[serde(default)]
    pub index: i64,
    pub agent: String,
    #[serde(default)]
    pub task: String,
    pub parent_tool_call_id: Option<String>,
    #[serde(default)]
    pub progress: Value,
    pub session_file: Option<String>,
}

/// A session event or notification, tagged by its `type`. Frames this
/// client doesn't model (or that don't match the modelled shape, so a newer
/// omp never stops the client) stay available as [`Event::Unknown`].
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Event {
    AgentStart,
    TurnStart,
    MessageStart {
        #[serde(default)]
        message: Value,
        message_id: Option<String>,
    },
    MessageUpdate {
        /// The `role` of the accumulated `message`.
        #[serde(rename = "message", default, deserialize_with = "role")]
        role: String,
        #[serde(rename = "assistantMessageEvent")]
        event: AssistantEvent,
        message_id: Option<String>,
    },
    MessageEnd {
        #[serde(default)]
        message: Value,
        message_id: Option<String>,
    },
    ToolExecutionStart { tool_call_id: String, tool_name: String, args: Option<Value>, intent: Option<String> },
    ToolExecutionUpdate { tool_call_id: String, tool_name: String, args: Option<Value>, partial_result: Option<Value> },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: Option<Value>,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(rename = "auto_compaction_end", alias = "manual_compaction_end")]
    CompactionEnd {
        result: Option<CompactionResult>,
        #[serde(default)]
        aborted: bool,
        #[serde(default)]
        skipped: bool,
        error_message: Option<String>,
    },
    ThinkingLevelChanged { thinking_level: Option<String> },
    TodoReminder {
        #[serde(default)]
        todos: Vec<TodoItem>,
    },
    TodoAutoClear,
    Notice {
        #[serde(default)]
        level: String,
        #[serde(default)]
        message: String,
        source: Option<String>,
    },
    PromptResult {
        id: Option<String>,
        #[serde(default)]
        agent_invoked: bool,
        status: PromptStatus,
        error: Option<PromptError>,
        #[serde(default)]
        session_settled: bool,
    },
    SessionSettled,
    #[serde(deserialize_with = "payload")]
    SubagentLifecycle(SubagentLifecycle),
    #[serde(deserialize_with = "payload")]
    SubagentProgress(SubagentProgress),
    ConfigUpdate { model: Option<ModelInfo>, thinking_level: Option<String> },
    SessionInfoUpdate {
        title: Option<String>,
        #[serde(default)]
        session_id: String,
    },
    /// Any other frame, raw.
    #[serde(skip)]
    Unknown(Value),
}

fn role<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    struct Message {
        #[serde(default)]
        role: String,
    }
    Message::deserialize(d).map(|m| m.role)
}

fn payload<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    #[derive(Deserialize)]
    struct Payload<T> {
        payload: T,
    }
    Payload::deserialize(d).map(|p| p.payload)
}

impl Event {
    /// Decodes one event frame; see [`Event::Unknown`].
    pub fn from_json(frame: Value) -> Event {
        Event::deserialize(&frame).unwrap_or(Event::Unknown(frame))
    }
}

/// An event or UI request frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    Event(Event),
    Ui(UiRequest),
}

impl Frame {
    /// A UI request this client doesn't model (status lines, widgets, …)
    /// stays an [`Event::Unknown`].
    pub fn from_json(frame: Value) -> Frame {
        match frame["type"] == "extension_ui_request" {
            true => UiRequest::deserialize(&frame).map_or_else(|_| Frame::Event(Event::Unknown(frame)), Frame::Ui),
            false => Frame::Event(Event::from_json(frame)),
        }
    }
}

/// One question of an `ask` UI request.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AskQuestion {
    pub id: String,
    pub question: String,
    pub header: Option<String>,
    #[serde(default)]
    pub options: Vec<AskOption>,
    #[serde(default)]
    pub multi: bool,
    pub recommended: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct AskOption {
    pub label: String,
    pub description: Option<String>,
}

/// An `extension_ui_request`. Dialogs (`select`, `confirm`, `input`,
/// `editor`, `ask`) wait for a [`UiResponse`](super::UiResponse); the rest
/// are fire and forget.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "method", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum UiRequest {
    Select { id: String, title: String, options: Vec<String>, timeout: Option<u64> },
    Confirm {
        id: String,
        title: String,
        #[serde(default)]
        message: String,
        timeout: Option<u64>,
    },
    Input {
        id: String,
        title: String,
        placeholder: Option<String>,
        timeout: Option<u64>,
        #[serde(default)]
        secret: bool,
    },
    Editor { id: String, title: String, prefill: Option<String> },
    Ask { id: String, questions: Vec<AskQuestion>, timeout: Option<u64> },
    /// omp closed the dialog `target_id` itself (timeout or abort).
    Cancel { id: String, target_id: String },
    Notify { id: String, message: String, notify_type: Option<String> },
    SetTitle { id: String, title: String },
    #[serde(rename = "open_url")]
    OpenUrl { id: String, url: String, launch_url: Option<String>, instructions: Option<String> },
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
            | UiRequest::SetTitle { id, .. }
            | UiRequest::OpenUrl { id, .. } => id,
        }
    }

    /// Whether omp waits for an answer.
    pub fn is_dialog(&self) -> bool {
        matches!(self, UiRequest::Select { .. } | UiRequest::Confirm { .. } | UiRequest::Input { .. } | UiRequest::Editor { .. } | UiRequest::Ask { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tagged_events_decode_and_mismatched_shapes_stay_unknown() {
        let lifecycle = json!({"type":"subagent_lifecycle","payload":{"id":"s","agent":"a","status":"started"}});
        let Event::SubagentLifecycle(l) = Event::from_json(lifecycle) else { panic!() };
        assert_eq!((l.id.as_str(), l.status), ("s", SubagentStatus::Started));
        let manual = json!({"type":"manual_compaction_end","skipped":true});
        assert_eq!(Event::from_json(manual), Event::CompactionEnd { result: None, aborted: false, skipped: true, error_message: None });
        let bad = json!({"type":"prompt_result","status":"exploded"});
        assert_eq!(Event::from_json(bad.clone()), Event::Unknown(bad));
    }
}
