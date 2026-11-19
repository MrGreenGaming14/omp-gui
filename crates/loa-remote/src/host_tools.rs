//! `remote_bash` and `remote_file` as host tools, for harnesses that take
//! the app's tools that way instead of MCP servers (omp). Same logic,
//! limits, approvals and audit as the MCP server.

use std::sync::Arc;

use async_trait::async_trait;
use loa_adapter::{HostToolSpec, HostTools};
use rmcp::model::{CallToolResult, ContentBlock};
use tokio_util::sync::CancellationToken;

use crate::RemoteService;
use crate::mcp::{self, tools};

/// The two tools as host tool specs, with the MCP server's JSON Schemas.
pub fn specs() -> Vec<HostToolSpec> {
    tools()
        .into_iter()
        .map(|tool| HostToolSpec {
            name: tool.name.to_string(),
            description: tool.description.as_deref().unwrap_or_default().to_owned(),
            input_schema: serde_json::Value::Object((*tool.input_schema).clone()).to_string(),
        })
        .collect()
}

/// Runs `remote_bash` and `remote_file` calls coming from a harness.
#[derive(Clone)]
pub struct RemoteHostTools {
    service: Arc<RemoteService>,
}

impl RemoteHostTools {
    pub fn new(service: Arc<RemoteService>) -> Self {
        Self { service }
    }
}

/// The result as the model reads it: the structured JSON, else the text blocks.
fn text(result: &CallToolResult) -> String {
    if let Some(value) = &result.structured_content {
        return serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    }
    result
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl HostTools for RemoteHostTools {
    async fn call(&self, session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        let arguments = match serde_json::from_str::<serde_json::Value>(args) {
            Ok(serde_json::Value::Object(map)) => Some(map),
            Ok(serde_json::Value::Null) => None,
            Ok(other) => return Err(format!("arguments must be a JSON object, got {other}")),
            Err(e) => return Err(format!("arguments are not JSON: {e}")),
        };
        // Host tool calls carry no cancellation; the call runs to its own timeout.
        let result = mcp::dispatch(&self.service, session_id, tool, arguments, &CancellationToken::new())
            .await
            .map_err(|e| e.message.to_string())?;
        let out = text(&result);
        if result.is_error == Some(true) { Err(out) } else { Ok(out) }
    }
}
