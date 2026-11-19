//! Loopback OpenAI compatible endpoint that stands in for a model.
//!
//! omp runs its tools only from an agent turn. This endpoint is the "model" of
//! those turns: when the newest user message carries a call marker it answers
//! with exactly that tool call, after the tool result it ends the turn, and any
//! other request (omp's startup probes, compaction) gets a short text reply. It
//! never forwards anything to a real provider.

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::Result;

/// Prefix of the prompt text that asks the provider for one tool call.
const CALL_PREFIX: &str = "loa-omp-bridge-call ";

/// Text the provider answers with when it is not asked for a tool call.
const DONE_TEXT: &str = "Done.";

/// One requested tool call, carried in the prompt text after [`CALL_PREFIX`].
#[derive(Serialize)]
struct CallMarker<'a> {
    id: &'a str,
    tool: &'a str,
    arguments: &'a Map<String, Value>,
}

#[derive(Deserialize)]
struct OwnedCallMarker {
    id: String,
    tool: String,
    arguments: Map<String, Value>,
}

/// Prompt text that makes the provider emit the call `id` of `tool` with `arguments`.
pub(crate) fn call_prompt(id: &str, tool: &str, arguments: &Map<String, Value>) -> Result<String> {
    let mut prompt = String::from(CALL_PREFIX);
    prompt.push_str(&serde_json::to_string(&CallMarker {
        id,
        tool,
        arguments,
    })?);
    Ok(prompt)
}

/// The running loopback endpoint. Stops when dropped.
pub(crate) struct ScriptedProvider {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl ScriptedProvider {
    /// Binds `127.0.0.1` on a free port. Requests must carry `Bearer <token>`.
    pub(crate) async fn start(token: &str) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let expected: Arc<str> = format!("Bearer {token}").into();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let expected = Arc::clone(&expected);
                tokio::spawn(async move {
                    let service = service_fn(move |request| handle(request, Arc::clone(&expected)));
                    if let Err(error) = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await
                    {
                        tracing::debug!(%error, "scripted provider connection ended");
                    }
                });
            }
        });
        Ok(Self { addr, task })
    }

    /// Base URL to configure as the provider's `baseUrl`.
    pub(crate) fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }
}

impl Drop for ScriptedProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(
    request: Request<Incoming>,
    expected: Arc<str>,
) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
    let authorized = request
        .headers()
        .get(AUTHORIZATION)
        .is_some_and(|value| value.as_bytes() == expected.as_bytes());
    if !authorized {
        return Ok(status(StatusCode::UNAUTHORIZED));
    }
    if request.method() != Method::POST || !request.uri().path().ends_with("/chat/completions") {
        return Ok(status(StatusCode::NOT_FOUND));
    }
    let body = match request.into_body().collect().await {
        Ok(body) => body.to_bytes(),
        Err(_) => return Ok(status(StatusCode::BAD_REQUEST)),
    };
    let Ok(stream) = completion(&body) else {
        return Ok(status(StatusCode::BAD_REQUEST));
    };
    let mut response = Response::new(Full::new(stream));
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(response)
}

fn status(code: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = code;
    response
}

/// Builds the server sent event stream answering one chat completions request body.
fn completion(body: &[u8]) -> Result<Bytes> {
    let request: Value = serde_json::from_slice(body)?;
    let model = request.get("model").and_then(Value::as_str).unwrap_or("scripted");
    let call = if request.get("tools").is_some() {
        request
            .get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| messages.last())
            .and_then(requested_call)
    } else {
        None
    };

    let mut out = Vec::with_capacity(512);
    let mut event = |choice: Value| -> Result<()> {
        out.extend_from_slice(b"data: ");
        serde_json::to_writer(
            &mut out,
            &json!({
                "id": "loa-omp-bridge",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": model,
                "choices": [choice],
            }),
        )?;
        out.extend_from_slice(b"\n\n");
        Ok(())
    };
    match call {
        Some(call) => {
            event(json!({
                "index": 0,
                "delta": {
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.tool,
                            "arguments": serde_json::to_string(&call.arguments)?,
                        },
                    }],
                },
                "finish_reason": null,
            }))?;
            event(json!({"index": 0, "delta": {}, "finish_reason": "tool_calls"}))?;
        }
        None => {
            event(json!({
                "index": 0,
                "delta": {"role": "assistant", "content": DONE_TEXT},
                "finish_reason": null,
            }))?;
            event(json!({"index": 0, "delta": {}, "finish_reason": "stop"}))?;
        }
    }
    out.extend_from_slice(b"data: [DONE]\n\n");
    Ok(Bytes::from(out))
}

/// The call marker in a user message, if the message carries one.
fn requested_call(message: &Value) -> Option<OwnedCallMarker> {
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let content = message.get("content")?;
    let marker = |text: &str| {
        text.strip_prefix(CALL_PREFIX)
            .and_then(|rest| serde_json::from_str::<OwnedCallMarker>(rest).ok())
    };
    match content {
        Value::String(text) => marker(text),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .find_map(marker),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(stream: &Bytes) -> Vec<Value> {
        let text = std::str::from_utf8(stream).unwrap();
        assert!(text.ends_with("data: [DONE]\n\n"));
        text.split("\n\n")
            .filter_map(|event| event.strip_prefix("data: "))
            .filter(|data| *data != "[DONE]")
            .map(|data| serde_json::from_str(data).unwrap())
            .collect()
    }

    fn agent_request(last: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "model": "scripted",
            "stream": true,
            "tools": [{"type": "function", "function": {"name": "edit"}}],
            "messages": [{"role": "system", "content": "prompt"}, last],
        }))
        .unwrap()
    }

    #[test]
    fn marked_prompt_becomes_exactly_that_tool_call() {
        let mut arguments = Map::new();
        arguments.insert("input".into(), json!("[a.txt#B6C1]\nPUT 2.=2:\n+BETA\n"));
        let prompt = call_prompt("call-7", "edit", &arguments).unwrap();
        let request = agent_request(json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "<system-reminder>cwd</system-reminder>"},
                {"type": "text", "text": prompt},
            ],
        }));

        let events = events(&completion(&request).unwrap());
        let call = &events[0]["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(call["id"], "call-7");
        assert_eq!(call["function"]["name"], "edit");
        let sent: Map<String, Value> =
            serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap();
        assert_eq!(sent, arguments);
        assert_eq!(events[1]["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn tool_result_ends_the_turn_instead_of_repeating_the_call() {
        let request = agent_request(json!({"role": "tool", "tool_call_id": "call-7", "content": "ok"}));
        let events = events(&completion(&request).unwrap());
        assert_eq!(events[0]["choices"][0]["delta"]["content"], DONE_TEXT);
        assert_eq!(events[1]["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn requests_without_tools_never_get_a_tool_call() {
        let prompt = call_prompt("call-1", "edit", &Map::new()).unwrap();
        let request = serde_json::to_vec(&json!({
            "model": "scripted",
            "messages": [{"role": "user", "content": prompt}],
        }))
        .unwrap();
        let events = events(&completion(&request).unwrap());
        assert!(events[0]["choices"][0]["delta"].get("tool_calls").is_none());
    }

    #[test]
    fn unmarked_user_text_gets_no_tool_call() {
        let request = agent_request(json!({"role": "user", "content": "please edit a.txt"}));
        let events = events(&completion(&request).unwrap());
        assert!(events[0]["choices"][0]["delta"].get("tool_calls").is_none());
    }
}
