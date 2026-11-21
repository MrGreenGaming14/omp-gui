//! The SDK MCP server (`"type": "sdk"` in the MCP config) serving the app's
//! host tools: Claude Code forwards its JSON-RPC messages inside
//! `mcp_message` control requests on the stdio it already uses.

use loa_contracts::harness::{HostToolSpec, HostTools};
use serde_json::{Value, json};

/// The host tool server's name; Claude Code shows its tools as `mcp__loa-host__<tool>`.
pub const HOST_SERVER: &str = "loa-host";

/// MCP revision answered when the client asks for none.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Answers one JSON-RPC message; notifications get an empty result as the ack the control protocol needs.
pub async fn handle(tools: &[HostToolSpec], handler: Option<&dyn HostTools>, session_id: &str, message: &Value) -> Value {
    let method = message["method"].as_str().unwrap_or_default();
    let result = match method {
        "initialize" => {
            let version = message["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION);
            let server = json!({"name": HOST_SERVER, "version": env!("CARGO_PKG_VERSION")});
            json!({"protocolVersion": version, "capabilities": {"tools": {"listChanged": false}}, "serverInfo": server})
        }
        "ping" => json!({}),
        "tools/list" => {
            let list: Vec<Value> = tools
                .iter()
                .map(|tool| {
                    let schema = serde_json::from_str::<Value>(&tool.input_schema).ok().filter(Value::is_object);
                    let schema = schema.unwrap_or_else(|| json!({"type": "object"}));
                    json!({"name": tool.name, "description": tool.description, "inputSchema": schema})
                })
                .collect();
            json!({ "tools": list })
        }
        "tools/call" => {
            let name = message["params"]["name"].as_str().unwrap_or_default();
            let outcome = match handler {
                _ if !tools.iter().any(|t| t.name == name) => Err(format!("unknown tool `{name}`")),
                None => Err("host tools have no handler".to_owned()),
                Some(handler) => {
                    let args = message["params"].get("arguments").map_or_else(|| "{}".to_owned(), Value::to_string);
                    handler.call(session_id, name, &args).await
                }
            };
            let is_error = outcome.is_err();
            json!({"content": [{"type": "text", "text": outcome.unwrap_or_else(|e| e)}], "isError": is_error})
        }
        _ if method.starts_with("notifications/") => json!({}),
        _ => return error(message, format!("method `{method}` not found")),
    };
    json!({"jsonrpc": "2.0", "id": message.get("id").unwrap_or(&Value::Null), "result": result})
}

/// A method-not-found JSON-RPC error answering `message`.
pub fn error(message: &Value, text: String) -> Value {
    let id = message.get("id").unwrap_or(&Value::Null);
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": text}})
}
