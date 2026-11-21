//! Loopback OpenAI compatible endpoint standing in for the model of omp's
//! turns: a user message carrying a call marker is answered with exactly that
//! tool call, anything else (the tool result, startup probes) with a short
//! text. Nothing is ever forwarded to a real provider.

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Response, StatusCode, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const CALL_PREFIX: &str = "loa-omp-bridge-call ";
const DONE_TEXT: &str = "Done.";

/// One requested tool call, carried in the prompt text after [`CALL_PREFIX`].
#[derive(Serialize, Deserialize)]
struct CallMarker {
    id: String,
    tool: String,
    arguments: Map<String, Value>,
}

/// Prompt text that makes the provider emit the call `id` of `tool` with `arguments`.
pub(crate) fn call_prompt(id: &str, tool: &str, arguments: Map<String, Value>) -> String {
    let marker = CallMarker { id: id.into(), tool: tool.into(), arguments };
    format!("{CALL_PREFIX}{}", serde_json::to_string(&marker).unwrap_or_default())
}

/// The running endpoint; stops when dropped.
pub(crate) struct ScriptedProvider {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl ScriptedProvider {
    /// Binds `127.0.0.1` on a free port. Requests must carry `Bearer <token>`.
    pub(crate) async fn start(token: &str) -> std::io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let addr = listener.local_addr()?;
        let expected: Arc<str> = format!("Bearer {token}").into();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let expected = expected.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| handle(request, expected.clone()));
                    if let Err(error) = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await {
                        tracing::debug!(%error, "scripted provider connection ended");
                    }
                });
            }
        });
        Ok(Self { addr, task })
    }

    pub(crate) fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }
}

impl Drop for ScriptedProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(request: Request<Incoming>, expected: Arc<str>) -> Result<Response<Full<Bytes>>, Infallible> {
    let status = |code| {
        let mut response = Response::new(Full::new(Bytes::new()));
        *response.status_mut() = code;
        Ok(response)
    };
    if request.headers().get(AUTHORIZATION).is_none_or(|value| value.as_bytes() != expected.as_bytes()) {
        return status(StatusCode::UNAUTHORIZED);
    }
    if request.method() != Method::POST || !request.uri().path().ends_with("/chat/completions") {
        return status(StatusCode::NOT_FOUND);
    }
    let Some(stream) = request.into_body().collect().await.ok().and_then(|body| completion(&body.to_bytes())) else {
        return status(StatusCode::BAD_REQUEST);
    };
    let mut response = Response::new(Full::new(stream));
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response.headers_mut().insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(response)
}

/// The server sent event stream answering one chat completions request body.
fn completion(body: &[u8]) -> Option<Bytes> {
    let request: Value = serde_json::from_slice(body).ok()?;
    let model = request["model"].as_str().unwrap_or("scripted");
    let call = request.get("tools").and_then(|_| request["messages"].as_array()?.last().and_then(requested_call));
    let (delta, finish) = match call {
        Some(call) => {
            let function = json!({"name": call.tool, "arguments": serde_json::to_string(&call.arguments).ok()?});
            (json!({"role": "assistant", "tool_calls": [{"index": 0, "id": call.id, "type": "function", "function": function}]}), "tool_calls")
        }
        None => (json!({"role": "assistant", "content": DONE_TEXT}), "stop"),
    };
    let mut out = String::new();
    for choice in [json!({"index": 0, "delta": delta, "finish_reason": null}), json!({"index": 0, "delta": {}, "finish_reason": finish})] {
        let chunk = json!({"id": "loa-omp-bridge", "object": "chat.completion.chunk", "created": 0, "model": model, "choices": [choice]});
        out.push_str(&format!("data: {chunk}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    Some(Bytes::from(out))
}

/// The call marker in a user message, if it carries one.
fn requested_call(message: &Value) -> Option<CallMarker> {
    if message["role"] != "user" {
        return None;
    }
    let marker = |text: &str| serde_json::from_str(text.strip_prefix(CALL_PREFIX)?).ok();
    match &message["content"] {
        Value::String(text) => marker(text),
        Value::Array(parts) => parts.iter().filter_map(|part| part["text"].as_str()).find_map(marker),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_marked_user_message_of_an_agent_request_gets_its_tool_call() {
        let args: Map<String, Value> = [("input".to_owned(), json!("[a.txt#B6C1]\n+BETA\n"))].into_iter().collect();
        let marked = json!([{"type": "text", "text": "<system-reminder/>"}, {"type": "text", "text": call_prompt("c7", "edit", args.clone())}]);
        let tools = json!([{"type": "function", "function": {"name": "edit"}}]);
        for (tools, last, call) in [
            (tools.clone(), json!({"role": "user", "content": marked}), true),
            (tools.clone(), json!({"role": "tool", "tool_call_id": "c7", "content": "ok"}), false),
            (tools, json!({"role": "user", "content": "please edit a.txt"}), false),
            (Value::Null, json!({"role": "user", "content": call_prompt("c1", "edit", Map::new())}), false),
        ] {
            let mut request = json!({"model": "scripted", "messages": [{"role": "system", "content": "p"}, last]});
            if !tools.is_null() {
                request["tools"] = tools;
            }
            let stream = completion(&serde_json::to_vec(&request).unwrap()).unwrap();
            let text = std::str::from_utf8(&stream).unwrap();
            assert!(text.ends_with("data: [DONE]\n\n"));
            let events: Vec<Value> = text.split("\n\n").filter_map(|e| serde_json::from_str(e.strip_prefix("data: ")?).ok()).collect();
            let delta = &events[0]["choices"][0]["delta"];
            if call {
                let sent = &delta["tool_calls"][0];
                assert_eq!((&sent["id"], &sent["function"]["name"]), (&json!("c7"), &json!("edit")));
                assert_eq!(serde_json::from_str::<Map<String, Value>>(sent["function"]["arguments"].as_str().unwrap()).unwrap(), args);
                assert_eq!(events[1]["choices"][0]["finish_reason"], "tool_calls");
            } else {
                assert_eq!((delta.get("tool_calls"), &delta["content"]), (None, &json!(DONE_TEXT)));
            }
        }
    }
}
