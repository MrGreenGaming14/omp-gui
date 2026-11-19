//! The MCP server that serves the app's host tools to Claude Code.
//!
//! It is an SDK MCP server: the MCP config names it with `"type": "sdk"`,
//! and Claude Code forwards each JSON-RPC message to the host inside an
//! `mcp_message` control request on the same stdio it already uses. No
//! extra process or port is involved.

use loa_adapter::{HostToolSpec, HostTools};
use serde_json::{Value, json};

/// Name of the host tool server in the MCP config. Claude Code shows its
/// tools as `mcp__loa-host__<tool>`.
pub const HOST_SERVER: &str = "loa-host";

/// MCP revision answered when the client asks for none.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Answers one JSON-RPC message sent to the host tool server. Notifications
/// get an empty result, which the control protocol needs as an ack.
pub async fn handle(
    tools: &[HostToolSpec],
    handler: Option<&dyn HostTools>,
    session_id: &str,
    message: &Value,
) -> Value {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
    let result = match method {
        "initialize" => {
            let version = message.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION);
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": HOST_SERVER, "version": env!("CARGO_PKG_VERSION")},
            })
        }
        "ping" => json!({}),
        "tools/list" => {
            let list: Vec<Value> = tools
                .iter()
                .map(|tool| {
                    let schema = serde_json::from_str::<Value>(&tool.input_schema)
                        .ok()
                        .filter(Value::is_object)
                        .unwrap_or_else(|| json!({"type": "object"}));
                    json!({"name": tool.name, "description": tool.description, "inputSchema": schema})
                })
                .collect();
            json!({ "tools": list })
        }
        "tools/call" => call(tools, handler, session_id, message).await,
        _ if method.starts_with("notifications/") => json!({}),
        _ => {
            return json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("method `{method}` not found")},
            });
        }
    };
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Error for a server name the host doesn't serve.
pub fn unknown_server(server: &str, message: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": message.get("id").cloned().unwrap_or(Value::Null),
        "error": {"code": -32601, "message": format!("server `{server}` not found")},
    })
}

async fn call(tools: &[HostToolSpec], handler: Option<&dyn HostTools>, session_id: &str, message: &Value) -> Value {
    let name = message.pointer("/params/name").and_then(Value::as_str).unwrap_or_default();
    let outcome = match handler {
        _ if !tools.iter().any(|t| t.name == name) => Err(format!("unknown tool `{name}`")),
        None => Err("host tools have no handler".to_owned()),
        Some(handler) => {
            let args = message.pointer("/params/arguments").map_or_else(|| "{}".to_owned(), Value::to_string);
            handler.call(session_id, name, &args).await
        }
    };
    let (text, is_error) = match outcome {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}
