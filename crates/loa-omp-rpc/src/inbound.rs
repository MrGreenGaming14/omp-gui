//! Non-command frames the host writes: answers to UI requests and results of
//! host tool and host URI callbacks.

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
        match self {
            UiResponse::Value { id, value } => json!({"type": "extension_ui_response", "id": id, "value": value}),
            UiResponse::Confirmed { id, confirmed } => {
                json!({"type": "extension_ui_response", "id": id, "confirmed": confirmed})
            }
            UiResponse::Cancelled { id, timed_out: false } => {
                json!({"type": "extension_ui_response", "id": id, "cancelled": true})
            }
            UiResponse::Cancelled { id, timed_out: true } => {
                json!({"type": "extension_ui_response", "id": id, "cancelled": true, "timedOut": true})
            }
            UiResponse::Answers { id, answers } => {
                json!({"type": "extension_ui_response", "id": id, "answers": answers})
            }
        }
    }
}

/// Outcome of a host tool call.
#[derive(Clone, Debug, PartialEq)]
pub struct HostToolOutput {
    /// Shown to the model as the tool result text.
    pub text: String,
    /// Surfaces `text` as a tool error.
    pub is_error: bool,
    /// Optional structured details kept with the result.
    pub details: Option<Value>,
}

impl HostToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: false, details: None }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self { text: text.into(), is_error: true, details: None }
    }

    fn payload(&self) -> Value {
        let mut payload = json!({"content": [{"type": "text", "text": self.text}]});
        if let (Some(details), Some(map)) = (&self.details, payload.as_object_mut()) {
            map.insert("details".to_owned(), details.clone());
        }
        payload
    }

    pub(crate) fn result_json(&self, id: &str) -> Value {
        let mut frame = json!({"type": "host_tool_result", "id": id, "result": self.payload()});
        if self.is_error
            && let Some(map) = frame.as_object_mut()
        {
            map.insert("isError".to_owned(), Value::Bool(true));
        }
        frame
    }

    pub(crate) fn update_json(&self, id: &str) -> Value {
        json!({"type": "host_tool_update", "id": id, "partialResult": self.payload()})
    }
}

/// Outcome of a host URI request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostUriOutput {
    /// A successful read.
    Content { content: String, content_type: Option<String>, immutable: Option<bool> },
    /// A successful write.
    Written,
    Error(String),
}

impl HostUriOutput {
    pub(crate) fn result_json(&self, id: &str) -> Value {
        match self {
            HostUriOutput::Content { content, content_type, immutable } => {
                let mut frame = json!({"type": "host_uri_result", "id": id, "content": content});
                if let Some(map) = frame.as_object_mut() {
                    if let Some(content_type) = content_type {
                        map.insert("contentType".to_owned(), Value::String(content_type.clone()));
                    }
                    if let Some(immutable) = immutable {
                        map.insert("immutable".to_owned(), Value::Bool(*immutable));
                    }
                }
                frame
            }
            HostUriOutput::Written => json!({"type": "host_uri_result", "id": id}),
            HostUriOutput::Error(error) => {
                json!({"type": "host_uri_result", "id": id, "isError": true, "error": error})
            }
        }
    }
}
