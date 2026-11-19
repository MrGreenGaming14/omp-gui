//! Registered MCP servers' tools as host tools, for harnesses that take the
//! app's tools that way instead of connecting to MCP servers (omp). Calls go
//! through the [`Manager`], so the per project toggles and peer grants of the
//! filter apply to every call.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use loa_adapter::{HostToolContent, HostToolSpec, HostTools};
use rmcp::model::{CallToolResult, ContentBlock, ResourceContents};

use crate::{Manager, Status, ToolFilter};

/// The host tool name of `tool` of `server`: `server__tool`, with any
/// character a model API refuses in tool names replaced by `_`. Not
/// `mcp__…`: omp treats tools named like its own MCP tools as discoverable
/// and may mount them as `xd://` devices the model never sees.
pub fn host_tool_name(server: &str, tool: &str) -> String {
    format!("{server}__{tool}")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

/// The tools of every running server that the filter allows, each routed
/// back to its server. A snapshot: a session gets the tools running when it
/// starts.
#[derive(Clone)]
pub struct McpHostTools {
    manager: Manager,
    filter: Arc<dyn ToolFilter>,
    specs: Vec<HostToolSpec>,
    routes: HashMap<String, (String, String)>,
}

impl std::fmt::Debug for McpHostTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpHostTools").field("tools", &self.routes.keys().collect::<Vec<_>>()).finish()
    }
}

impl McpHostTools {
    pub fn new(manager: Manager, filter: Arc<dyn ToolFilter>) -> Self {
        let mut specs = Vec::new();
        let mut routes = HashMap::new();
        for server in manager.view(&*filter) {
            if server.status != Status::Running {
                continue;
            }
            for view in server.tools.into_iter().filter(|view| view.allowed) {
                let tool = view.tool;
                let name = host_tool_name(&server.spec.name, &tool.name);
                if routes.contains_key(&name) {
                    tracing::warn!(tool = name, "two MCP tools map to the same host tool name; keeping the first");
                    continue;
                }
                let description = match tool.description.as_deref() {
                    Some(text) if !text.is_empty() => format!("[{}] {text}", server.spec.name),
                    _ => format!("Tool `{}` of the MCP server `{}`", tool.name, server.spec.name),
                };
                specs.push(HostToolSpec {
                    name: name.clone(),
                    description,
                    input_schema: serde_json::Value::Object((*tool.input_schema).clone()).to_string(),
                });
                routes.insert(name, (server.spec.name.clone(), tool.name.to_string()));
            }
        }
        Self { manager, filter, specs, routes }
    }

    pub fn specs(&self) -> &[HostToolSpec] {
        &self.specs
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    async fn run(&self, tool: &str, args: &str) -> Result<CallToolResult, String> {
        let (server, name) = self.routes.get(tool).ok_or_else(|| format!("unknown MCP tool `{tool}`"))?;
        let arguments = match serde_json::from_str::<serde_json::Value>(args) {
            Ok(serde_json::Value::Object(map)) => Some(map),
            Ok(serde_json::Value::Null) => None,
            Ok(other) => return Err(format!("arguments must be a JSON object, got {other}")),
            Err(e) => return Err(format!("arguments are not JSON: {e}")),
        };
        self.manager.call_tool(server, name, arguments, &*self.filter).await.map_err(|e| e.to_string())
    }
}

/// Text of a content block the model can read as text.
fn block_text(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(text) => Some(text.text.clone()),
        ContentBlock::Resource(embedded) => Some(match &embedded.resource {
            ResourceContents::TextResourceContents { text, .. } => text.clone(),
            ResourceContents::BlobResourceContents { uri, .. } => format!("[binary resource {uri}]"),
            _ => "[resource]".to_owned(),
        }),
        ContentBlock::ResourceLink(link) => Some(format!("[resource {}]", link.uri)),
        ContentBlock::Audio(audio) => Some(format!("[{} audio]", audio.mime_type)),
        ContentBlock::Image(_) => None,
        _ => Some("[unsupported content]".to_owned()),
    }
}

/// The blocks of a result, structured content first when the server gave no text.
fn content(result: &CallToolResult) -> Vec<HostToolContent> {
    let mut out = Vec::with_capacity(result.content.len());
    for block in &result.content {
        if let ContentBlock::Image(image) = block {
            match STANDARD.decode(image.data.as_bytes()) {
                Ok(data) => {
                    out.push(HostToolContent::Image { mime: image.mime_type.clone(), data: Bytes::from(data) });
                }
                Err(e) => out.push(HostToolContent::Text(format!("[undecodable {} image: {e}]", image.mime_type))),
            }
        } else if let Some(text) = block_text(block) {
            out.push(HostToolContent::Text(text));
        }
    }
    if out.is_empty()
        && let Some(value) = &result.structured_content
    {
        out.push(HostToolContent::Text(value.to_string()));
    }
    out
}

fn text(content: &[HostToolContent]) -> String {
    content
        .iter()
        .map(|block| match block {
            HostToolContent::Text(text) => text.clone(),
            HostToolContent::Image { mime, data } => format!("[{mime} image, {} bytes]", data.len()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl HostTools for McpHostTools {
    async fn call(&self, _session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        let result = self.run(tool, args).await?;
        let out = text(&content(&result));
        if result.is_error == Some(true) { Err(out) } else { Ok(out) }
    }

    async fn call_content(&self, _session_id: &str, tool: &str, args: &str) -> Result<Vec<HostToolContent>, String> {
        let result = self.run(tool, args).await?;
        let blocks = content(&result);
        if result.is_error == Some(true) { Err(text(&blocks)) } else { Ok(blocks) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_for_model_apis() {
        assert_eq!(host_tool_name("github.com", "list issues"), "github_com__list_issues");
        assert_eq!(host_tool_name("fs-2", "read_file"), "fs-2__read_file");
    }
}
