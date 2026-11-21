//! Non-command frames the host writes: answers to UI requests and results of
//! host tool callbacks.

use super::command::ImageContent;
use serde::Serialize;
use serde_json::{Value, json};

/// One answer to an `ask` question.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskAnswer {
    pub id: String,
    pub selected_options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_input: Option<String>,
}

/// An `extension_ui_response`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UiResponse {
    /// Answers `select`, `input` or `editor`.
    Value { id: String, value: String },
    /// Answers `confirm`.
    Confirmed { id: String, confirmed: bool },
    /// Dismisses any dialog. `timed_out` reports the host's own deadline.
    Cancelled { id: String, timed_out: bool },
    /// Answers `ask`, one entry per question in request order.
    Answers { id: String, answers: Vec<AskAnswer> },
}

impl UiResponse {
    pub(crate) fn to_json(&self) -> Value {
        let (id, answer) = match self {
            UiResponse::Value { id, value } => (id, json!({"value": value})),
            UiResponse::Confirmed { id, confirmed } => (id, json!({"confirmed": confirmed})),
            UiResponse::Cancelled { id, timed_out: false } => (id, json!({"cancelled": true})),
            UiResponse::Cancelled { id, timed_out: true } => (id, json!({"cancelled": true, "timedOut": true})),
            UiResponse::Answers { id, answers } => (id, json!({"answers": answers})),
        };
        let mut frame = json!({"type": "extension_ui_response", "id": id});
        if let (Value::Object(frame), Value::Object(answer)) = (&mut frame, answer) {
            frame.extend(answer);
        }
        frame
    }
}

/// One block of a host tool result: what the model sees.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ToolContent {
    Text(TextContent),
    /// `{"type":"image","data":<base64>,"mimeType":…}`, the image block of
    /// omp's tool results.
    Image(ImageContent),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename = "text")]
pub struct TextContent {
    pub text: String,
}

impl ToolContent {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextContent { text: text.into() })
    }
}

/// Outcome of a host tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct HostToolOutput {
    /// The result blocks shown to the model.
    pub content: Vec<ToolContent>,
    /// Surfaces the text blocks as a tool error.
    pub is_error: bool,
    /// Optional structured details kept with the result.
    pub details: Option<Value>,
}

impl HostToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        Self::blocks(vec![ToolContent::text(text)])
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { content: vec![ToolContent::text(text)], is_error: true, details: None }
    }

    /// A successful result of text and image blocks.
    pub fn blocks(content: Vec<ToolContent>) -> Self {
        Self { content, is_error: false, details: None }
    }

    pub(crate) fn result_json(&self, id: &str) -> Value {
        let mut result = json!({"content": self.content});
        if let Some(details) = &self.details {
            result["details"] = details.clone();
        }
        let mut frame = json!({"type": "host_tool_result", "id": id, "result": result});
        if self.is_error {
            frame["isError"] = Value::Bool(true);
        }
        frame
    }
}
