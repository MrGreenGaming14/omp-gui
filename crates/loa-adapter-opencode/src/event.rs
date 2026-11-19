//! Events of the OpenCode server (`GET /event`) and the server sent event
//! framing that carries them.
//!
//! Every event is `{"id", "type", "properties"}`. Objects the adapter keeps
//! verbatim (session info, message info, parts) stay `serde_json::Value`, so
//! fields this version doesn't know survive into `x-oc` attributes and `<ext>`.

use serde::Deserialize;
use serde_json::Value as Json;

/// One server event, decoded as far as the adapter needs.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// First event of every subscription.
    Connected,
    Heartbeat,
    SessionCreated { info: Json },
    SessionUpdated { info: Json },
    SessionDeleted { info: Json },
    /// `status` is `idle`, `busy` or `retry`; `raw` is the whole status object.
    SessionStatus { session_id: String, status: String, raw: Json },
    SessionIdle { session_id: String },
    SessionCompacted { session_id: String },
    SessionError { session_id: Option<String>, error: Option<Json> },
    MessageUpdated { session_id: String, info: Json },
    MessageRemoved { session_id: String, message_id: String },
    PartUpdated { session_id: String, part: Json },
    PartDelta(PartDelta),
    PartRemoved { session_id: String, message_id: String, part_id: String },
    TodoUpdated { session_id: String, todos: Vec<Todo> },
    PermissionAsked(PermissionRequest),
    PermissionReplied { session_id: String, request_id: String, reply: String },
    QuestionAsked(QuestionRequest),
    QuestionReplied { session_id: String, request_id: String, answers: Vec<Vec<String>> },
    QuestionRejected { session_id: String, request_id: String },
    /// Any other event type, with its properties.
    Other { kind: String, properties: Json },
}

/// `message.part.delta`: streamed text appended to one field of a part.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartDelta {
    #[serde(rename = "sessionID")]
    pub session_id: String,
    #[serde(rename = "messageID")]
    pub message_id: String,
    #[serde(rename = "partID")]
    pub part_id: String,
    pub field: String,
    pub delta: String,
}

/// One entry of the session's todo list.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Todo {
    pub content: String,
    /// `pending`, `in_progress`, `completed` or `cancelled`.
    pub status: String,
    #[serde(default)]
    pub priority: String,
}

/// The tool call a permission or question request belongs to.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct ToolRef {
    #[serde(rename = "messageID")]
    pub message_id: String,
    #[serde(rename = "callID")]
    pub call_id: String,
}

/// `permission.asked`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
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

/// One question of a `question.asked` request.
#[derive(Clone, Debug, PartialEq, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// `question.asked`: one request holding several questions, answered together.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct QuestionRequest {
    pub id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub questions: Vec<QuestionInfo>,
    #[serde(default)]
    pub tool: Option<ToolRef>,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    properties: Json,
}

#[derive(Deserialize)]
struct WithInfo {
    info: Json,
}

#[derive(Deserialize)]
struct SessionRef {
    #[serde(rename = "sessionID")]
    session_id: String,
}

#[derive(Deserialize)]
struct StatusProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    status: Json,
}

#[derive(Deserialize)]
struct ErrorProps {
    #[serde(rename = "sessionID", default)]
    session_id: Option<String>,
    #[serde(default)]
    error: Option<Json>,
}

#[derive(Deserialize)]
struct MessageProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    info: Json,
}

#[derive(Deserialize)]
struct MessageRemovedProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "messageID")]
    message_id: String,
}

#[derive(Deserialize)]
struct PartProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    part: Json,
}

#[derive(Deserialize)]
struct PartRemovedProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "messageID")]
    message_id: String,
    #[serde(rename = "partID")]
    part_id: String,
}

#[derive(Deserialize)]
struct TodoProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    todos: Vec<Todo>,
}

#[derive(Deserialize)]
struct RepliedProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "requestID")]
    request_id: String,
    reply: String,
}

#[derive(Deserialize)]
struct QuestionRepliedProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "requestID")]
    request_id: String,
    answers: Vec<Vec<String>>,
}

#[derive(Deserialize)]
struct RejectedProps {
    #[serde(rename = "sessionID")]
    session_id: String,
    #[serde(rename = "requestID")]
    request_id: String,
}

impl Event {
    /// Decodes the JSON payload of one server sent event.
    pub fn parse(data: &str) -> serde_json::Result<Event> {
        let Envelope { kind, properties: p } = serde_json::from_str(data)?;
        use serde_json::from_value as from;
        Ok(match kind.as_str() {
            "server.connected" => Event::Connected,
            "server.heartbeat" => Event::Heartbeat,
            "session.created" => Event::SessionCreated { info: serde_json::from_value::<WithInfo>(p)?.info },
            "session.updated" => Event::SessionUpdated { info: serde_json::from_value::<WithInfo>(p)?.info },
            "session.deleted" => Event::SessionDeleted { info: serde_json::from_value::<WithInfo>(p)?.info },
            "session.status" => {
                let StatusProps { session_id, status } = from(p)?;
                let name = status.get("type").and_then(Json::as_str).unwrap_or_default().to_owned();
                Event::SessionStatus { session_id, status: name, raw: status }
            }
            "session.idle" => Event::SessionIdle { session_id: serde_json::from_value::<SessionRef>(p)?.session_id },
            "session.compacted" => Event::SessionCompacted { session_id: serde_json::from_value::<SessionRef>(p)?.session_id },
            "session.error" => {
                let ErrorProps { session_id, error } = from(p)?;
                Event::SessionError { session_id, error }
            }
            "message.updated" => {
                let MessageProps { session_id, info } = from(p)?;
                Event::MessageUpdated { session_id, info }
            }
            "message.removed" => {
                let MessageRemovedProps { session_id, message_id } = from(p)?;
                Event::MessageRemoved { session_id, message_id }
            }
            "message.part.updated" => {
                let PartProps { session_id, part } = from(p)?;
                Event::PartUpdated { session_id, part }
            }
            "message.part.delta" => Event::PartDelta(from(p)?),
            "message.part.removed" => {
                let PartRemovedProps { session_id, message_id, part_id } = from(p)?;
                Event::PartRemoved { session_id, message_id, part_id }
            }
            "todo.updated" => {
                let TodoProps { session_id, todos } = from(p)?;
                Event::TodoUpdated { session_id, todos }
            }
            "permission.asked" => Event::PermissionAsked(from(p)?),
            "permission.replied" => {
                let RepliedProps { session_id, request_id, reply } = from(p)?;
                Event::PermissionReplied { session_id, request_id, reply }
            }
            "question.asked" => Event::QuestionAsked(from(p)?),
            "question.replied" => {
                let QuestionRepliedProps { session_id, request_id, answers } = from(p)?;
                Event::QuestionReplied { session_id, request_id, answers }
            }
            "question.rejected" => {
                let RejectedProps { session_id, request_id } = from(p)?;
                Event::QuestionRejected { session_id, request_id }
            }
            _ => Event::Other { kind, properties: p },
        })
    }

    /// The native session the event is about, when it names one.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Event::Connected | Event::Heartbeat => None,
            Event::SessionCreated { info } | Event::SessionUpdated { info } | Event::SessionDeleted { info } => {
                info.get("id").and_then(Json::as_str)
            }
            Event::SessionError { session_id, .. } => session_id.as_deref(),
            Event::SessionStatus { session_id, .. }
            | Event::SessionIdle { session_id }
            | Event::SessionCompacted { session_id }
            | Event::MessageUpdated { session_id, .. }
            | Event::MessageRemoved { session_id, .. }
            | Event::PartUpdated { session_id, .. }
            | Event::PartRemoved { session_id, .. }
            | Event::TodoUpdated { session_id, .. }
            | Event::PermissionReplied { session_id, .. }
            | Event::QuestionReplied { session_id, .. }
            | Event::QuestionRejected { session_id, .. } => Some(session_id),
            Event::PartDelta(d) => Some(&d.session_id),
            Event::PermissionAsked(r) => Some(&r.session_id),
            Event::QuestionAsked(r) => Some(&r.session_id),
            Event::Other { properties, .. } => properties.get("sessionID").and_then(Json::as_str),
        }
    }
}

/// Splits a `text/event-stream` body into event payloads.
///
/// Lines end in `\n` (a trailing `\r` is dropped). `data:` lines of one event
/// are joined with `\n`; a blank line ends the event. Other fields (`id`,
/// `event`, `retry`) and comments are ignored: the payload carries its type.
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    data: String,
    has_data: bool,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds a chunk and pushes every completed payload onto `out`.
    pub fn feed(&mut self, chunk: &[u8], out: &mut impl Extend<String>) {
        self.buf.extend_from_slice(chunk);
        let mut start = 0;
        while let Some(end) = self.buf[start..].iter().position(|b| *b == b'\n') {
            let mut line = &self.buf[start..start + end];
            start += end + 1;
            if let [rest @ .., b'\r'] = line {
                line = rest;
            }
            if line.is_empty() {
                if self.has_data {
                    out.extend(std::iter::once(std::mem::take(&mut self.data)));
                    self.has_data = false;
                }
                continue;
            }
            let (field, value) = match line.iter().position(|b| *b == b':') {
                Some(colon) => {
                    let value = &line[colon + 1..];
                    (&line[..colon], value.strip_prefix(b" ").unwrap_or(value))
                }
                None => (line, &[][..]),
            };
            if field == b"data" {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(&String::from_utf8_lossy(value));
                self.has_data = true;
            }
        }
        self.buf.drain(..start);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_joins_split_chunks_and_multiline_data() {
        let mut dec = SseDecoder::new();
        let mut out = Vec::new();
        dec.feed(b"data: {\"a\":", &mut out);
        assert!(out.is_empty());
        dec.feed(b"1}\r\n\r\n: comment\nevent: message\ndata: x\ndata: y\n", &mut out);
        assert_eq!(out, vec!["{\"a\":1}".to_owned()]);
        dec.feed(b"\n", &mut out);
        assert_eq!(out[1], "x\ny");
    }

    #[test]
    fn unknown_event_keeps_its_session() {
        let ev = Event::parse(r#"{"id":"e","type":"session.diff","properties":{"sessionID":"ses_1","diff":[]}}"#)
            .unwrap();
        assert_eq!(ev.session_id(), Some("ses_1"));
        assert!(matches!(ev, Event::Other { ref kind, .. } if kind == "session.diff"));
    }
}
