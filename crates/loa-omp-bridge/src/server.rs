use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Error, OutputBlock, Result, ToolExecutor, ToolOutput};

/// MCP server name reported to clients. Harnesses prefix tool names with it
/// (`mcp__omp__edit`).
const SERVER_NAME: &str = "omp";

const INSTRUCTIONS: &str = "omp's own tools. `edit` takes hashline patches: read a file with this \
server's `read` first and copy the `[path#TAG]` header it returns.";

/// MCP server handler that offers an executor's tools.
pub struct BridgeServer<E> {
    executor: Arc<E>,
    tools: Arc<[Tool]>,
}

impl<E> Clone for BridgeServer<E> {
    fn clone(&self) -> Self {
        Self {
            executor: Arc::clone(&self.executor),
            tools: Arc::clone(&self.tools),
        }
    }
}

impl<E: ToolExecutor> BridgeServer<E> {
    pub fn new(executor: Arc<E>) -> Self {
        let tools = executor
            .tools()
            .iter()
            .map(|spec| {
                Tool::new(
                    spec.name.clone(),
                    spec.description.clone(),
                    Arc::new(spec.input_schema.clone()),
                )
            })
            .collect();
        Self { executor, tools }
    }
}

impl<E: ToolExecutor> ServerHandler for BridgeServer<E> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools.to_vec()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if !self.tools.iter().any(|tool| tool.name == request.name) {
            return Err(McpError::invalid_params(
                format!("unknown tool: {}", request.name),
                None,
            ));
        }
        let arguments = request.arguments.unwrap_or_default();
        let result = match self.executor.call(&request.name, arguments, context.ct).await {
            Ok(output) => into_result(output),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error.to_string())]),
        };
        Ok(result.into())
    }
}

fn into_result(output: ToolOutput) -> CallToolResult {
    let content = output
        .content
        .into_iter()
        .map(|block| match block {
            OutputBlock::Text(text) => ContentBlock::text(text),
            OutputBlock::Image { data, mime_type } => ContentBlock::image(data, mime_type),
        })
        .collect();
    if output.is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

/// Serves `executor`'s tools over MCP on `reader`/`writer` until the client disconnects.
pub async fn serve<E, R, W>(executor: Arc<E>, reader: R, writer: W) -> Result<()>
where
    E: ToolExecutor,
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Send + Unpin + 'static,
{
    let running = BridgeServer::new(executor)
        .serve((reader, writer))
        .await
        .map_err(|error| Error::Mcp(error.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|error| Error::Mcp(error.to_string()))?;
    Ok(())
}

/// Serves `executor`'s tools over MCP on this process's stdin and stdout.
pub async fn serve_stdio<E: ToolExecutor>(executor: Arc<E>) -> Result<()> {
    let (stdin, stdout) = rmcp::transport::stdio();
    serve(executor, stdin, stdout).await
}
