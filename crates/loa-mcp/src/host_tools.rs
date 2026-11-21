//! Running servers' tools as host tools, for harnesses that take the app's
//! tools that way (omp). Calls go through the [`Manager`], so the filter's
//! project toggles and peer grants apply to every call.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use loa_contracts::harness::{HostToolSpec, HostTools, OutputBlock};
use loa_contracts::tools::ToolOutput;

use crate::{Manager, Status, ToolFilter, toolkit};

/// `server__tool`, with characters model APIs refuse replaced by `_`. Not
/// `mcp__…`: omp may mount tools named like its own MCP tools as `xd://` devices.
pub fn host_tool_name(server: &str, tool: &str) -> String {
    let name = format!("{server}__{tool}");
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect()
}

/// A snapshot of the allowed tools of every running server, each routed back to its server.
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
        let (mut specs, mut routes) = (Vec::new(), HashMap::new());
        for server in manager.view(&*filter).into_iter().filter(|server| server.status == Status::Running) {
            let server_name = &server.spec.name;
            for tool in server.tools.into_iter().filter(|view| view.allowed).map(|view| view.tool) {
                let name = host_tool_name(server_name, &tool.name);
                if routes.contains_key(&name) {
                    tracing::warn!(tool = name, "two MCP tools map to the same host tool name; keeping the first");
                    continue;
                }
                let description = match tool.description.as_deref() {
                    Some(text) if !text.is_empty() => format!("[{server_name}] {text}"),
                    _ => format!("Tool `{}` of the MCP server `{server_name}`", tool.name),
                };
                let input_schema = serde_json::Value::Object((*tool.input_schema).clone()).to_string();
                specs.push(HostToolSpec { name: name.clone(), description, input_schema });
                routes.insert(name, (server_name.clone(), tool.name.to_string()));
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

    async fn run(&self, tool: &str, args: &str) -> Result<ToolOutput, String> {
        let (server, name) = self.routes.get(tool).ok_or_else(|| format!("unknown MCP tool `{tool}`"))?;
        let arguments = match serde_json::from_str(args).map_err(|e| format!("arguments are not JSON: {e}"))? {
            serde_json::Value::Object(map) => Some(map),
            serde_json::Value::Null => None,
            other => return Err(format!("arguments must be a JSON object, got {other}")),
        };
        let result = self.manager.call_tool(server, name, arguments, &*self.filter).await.map_err(|e| e.to_string())?;
        let output = toolkit::to_output(result);
        if output.is_error { Err(toolkit::text(&output)) } else { Ok(output) }
    }
}

#[async_trait]
impl HostTools for McpHostTools {
    async fn call(&self, _session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        self.run(tool, args).await.map(|output| toolkit::text(&output))
    }

    async fn call_content(&self, _session_id: &str, tool: &str, args: &str) -> Result<Vec<OutputBlock>, String> {
        self.run(tool, args).await.map(|output| output.blocks)
    }
}
