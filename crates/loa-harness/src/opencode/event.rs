//! Events of `GET /event` (`{"type", "properties"}`) and the server sent event
//! framing that carries them. Session, message and part objects stay JSON so
//! fields this version doesn't know survive into `x-oc` and `<ext>`.

use serde::Deserialize;
use serde_json::Value as Json;

/// One server event; types the adapter doesn't model arrive as [`Event::Other`].
#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Event {
    Known(Known),
    Other {
        #[serde(rename = "type")]
        kind: String,
        #[serde(default)]
        properties: Json,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", content = "properties")]
pub enum Known {
    #[serde(rename = "session.created")]
    SessionCreated { info: Json },
    #[serde(rename = "session.updated")]
    SessionUpdated { info: Json },
    #[serde(rename = "session.status")]
    SessionStatus {
        #[serde(flatten)]
        at: Ref,
        status: Json,
    },
    #[serde(rename = "session.idle")]
    SessionIdle(Ref),
    #[serde(rename = "session.error")]
    SessionError {
        #[serde(flatten)]
        at: Ref,
        error: Option<Json>,
    },
    #[serde(rename = "message.updated")]
    MessageUpdated {
        #[serde(flatten)]
        at: Ref,
        info: Json,
    },
    #[serde(rename = "message.removed")]
    MessageRemoved(Ref),
    #[serde(rename = "message.part.updated")]
    PartUpdated {
        #[serde(flatten)]
        at: Ref,
        part: Json,
    },
    /// Streamed text appended to one `field` of a part.
    #[serde(rename = "message.part.delta")]
    PartDelta {
        #[serde(flatten)]
        at: Ref,
        field: String,
        delta: String,
    },
    #[serde(rename = "message.part.removed")]
    PartRemoved(Ref),
    #[serde(rename = "todo.updated")]
    TodoUpdated {
        #[serde(flatten)]
        at: Ref,
        todos: Vec<Todo>,
    },
    #[serde(rename = "permission.asked")]
    PermissionAsked(PermissionRequest),
    #[serde(rename = "permission.replied")]
    PermissionReplied {
        #[serde(flatten)]
        at: Ref,
        reply: String,
    },
    #[serde(rename = "question.asked")]
    QuestionAsked(QuestionRequest),
    #[serde(rename = "question.replied")]
    QuestionReplied {
        #[serde(flatten)]
        at: Ref,
        answers: Vec<Vec<String>>,
    },
    #[serde(rename = "question.rejected")]
    QuestionRejected(Ref),
}

/// The ids an event names; absent ones are empty.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Ref {
    #[serde(rename = "sessionID")]
    pub session: String,
    #[serde(rename = "messageID")]
    pub message: String,
    #[serde(rename = "partID")]
    pub part: String,
    #[serde(rename = "requestID")]
    pub request: String,
}

/// One entry of the session's todo list; `status` is `pending`, `in_progress`, `completed` or `cancelled`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Todo {
    pub content: String,
    pub status: String,
    #[serde(default)]
    pub priority: String,
}

/// The tool call a request belongs to.
#[derive(Clone, Debug, Deserialize)]
pub struct ToolRef {
    #[serde(rename = "callID")]
    pub call_id: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PermissionRequest {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    /// The permission key, e.g. `bash`, `edit`, `task`.
    pub permission: String,
    #[serde(default)]
    pub patterns: Vec<String>,
    #[serde(default)]
    pub metadata: Json,
    /// Patterns an "always" reply would allow from now on.
    #[serde(default)]
    pub always: Vec<String>,
    #[serde(default)]
    pub tool: Option<ToolRef>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QuestionInfo {
    pub question: String,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    #[serde(default)]
    pub multiple: Option<bool>,
    /// Typed answers allowed besides the options.
    #[serde(default)]
    pub custom: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QuestionOption {
    pub label: String,
}

/// `question.asked`: several questions answered together.
#[derive(Clone, Debug, Deserialize)]
pub struct QuestionRequest {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub questions: Vec<QuestionInfo>,
    #[serde(default)]
    pub tool: Option<ToolRef>,
}

impl Event {
    /// The native session the event is about, when it names one.
    pub fn session_id(&self) -> Option<&str> {
        let id = match self {
            Event::Other { properties, .. } => properties.get("sessionID")?.as_str()?,
            Event::Known(known) => match known {
                Known::SessionCreated { info } | Known::SessionUpdated { info } => info.get("id")?.as_str()?,
                Known::PermissionAsked(r) => &r.session_id,
                Known::QuestionAsked(r) => &r.session_id,
                Known::SessionIdle(at) | Known::MessageRemoved(at) | Known::PartRemoved(at) | Known::QuestionRejected(at) => &at.session,
                Known::SessionStatus { at, .. }
                | Known::SessionError { at, .. }
                | Known::MessageUpdated { at, .. }
                | Known::PartUpdated { at, .. }
                | Known::PartDelta { at, .. }
                | Known::TodoUpdated { at, .. }
                | Known::PermissionReplied { at, .. }
                | Known::QuestionReplied { at, .. } => &at.session,
            },
        };
        Some(id).filter(|id| !id.is_empty())
    }
}

/// Splits a `text/event-stream` body into `data` payloads: `data:` lines of
/// one event are joined with `\n`, a blank line ends it, other fields are ignored.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    data: Option<String>,
}

impl SseDecoder {
    /// Feeds a chunk and pushes every completed payload onto `out`.
    pub fn feed(&mut self, chunk: &[u8], out: &mut impl Extend<String>) {
        self.buf.extend_from_slice(chunk);
        let mut start = 0;
        while let Some(end) = self.buf[start..].iter().position(|b| *b == b'\n') {
            let line = &self.buf[start..start + end];
            start += end + 1;
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                out.extend(self.data.take());
            } else if let Some(value) = if line == b"data" { Some(&b""[..]) } else { line.strip_prefix(b"data:") } {
                let value = String::from_utf8_lossy(value.strip_prefix(b" ").unwrap_or(value));
                match &mut self.data {
                    Some(data) => {
                        data.push('\n');
                        data.push_str(&value);
                    }
                    None => self.data = Some(value.into_owned()),
                }
            }
        }
        self.buf.drain(..start);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn decoder_joins_split_chunks_and_multiline_data() {
        let mut dec = super::SseDecoder::default();
        let mut out = Vec::new();
        dec.feed(b"data: {\"a\":", &mut out);
        assert!(out.is_empty());
        dec.feed(b"1}\r\n\r\n: comment\nevent: message\ndata: x\ndata\ndata: y\n\n", &mut out);
        assert_eq!(out, ["{\"a\":1}", "x\n\ny"]);
    }
}
