//! The `remote` MCP server: `remote_bash` and `remote_file` over `rmcp`, on
//! stdio or any in-process byte stream.
//!
//! One [`RemoteMcp`] serves one calling session: its session id goes into
//! every audit row and approval request the server makes.

use std::borrow::Cow;
use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::{RequestContext, RoleServer, RunningService, ServiceExt};
use rmcp::{ErrorData, ServerHandler};
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::Error;
use crate::service::{BashArgs, FileArgs, RemoteService};

/// MCP server name.
pub const SERVER_NAME: &str = "remote";
/// Tool names.
pub const REMOTE_BASH: &str = "remote_bash";
pub const REMOTE_FILE: &str = "remote_file";

/// The `remote` server bound to one calling session.
#[derive(Clone)]
pub struct RemoteMcp {
    service: Arc<RemoteService>,
    session_id: Arc<str>,
}

fn schema<T: schemars::JsonSchema>() -> Arc<JsonObject> {
    let value = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default();
    match value {
        serde_json::Value::Object(map) => Arc::new(map),
        _ => Arc::new(JsonObject::new()),
    }
}

/// The two tools with their JSON Schemas.
pub fn tools() -> Vec<Tool> {
    let mut bash = Tool::new(
        REMOTE_BASH,
        "Run a shell command on a configured SSH host. Returns stdout, stderr and the exit \
         status. Subject to the host's limits and the user's approval.",
        schema::<BashArgs>(),
    );
    bash.annotations = Some(
        ToolAnnotations::new()
            .destructive(true)
            .open_world(true),
    );
    let mut file = Tool::new(
        REMOTE_FILE,
        "Read, list, write or edit a file on a configured SSH host. `edit` replaces one exact, \
         unique occurrence of `old` with `new`. Writes are atomic where the server allows.",
        schema::<FileArgs>(),
    );
    file.annotations = Some(ToolAnnotations::new().destructive(true).open_world(true));
    vec![bash, file]
}

fn parse<T: DeserializeOwned>(args: Option<JsonObject>) -> Result<T, ErrorData> {
    serde_json::from_value(serde_json::Value::Object(args.unwrap_or_default()))
        .map_err(|e| ErrorData::invalid_params(format!("bad arguments: {e}"), None))
}

fn tool_error(e: &Error) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(e.to_string())])
}

fn tool_ok<T: serde::Serialize>(value: &T) -> CallToolResult {
    match serde_json::to_value(value) {
        Ok(v) => CallToolResult::structured(v),
        Err(e) => CallToolResult::error(vec![ContentBlock::text(e.to_string())]),
    }
}

impl RemoteMcp {
    pub fn new(service: Arc<RemoteService>, session_id: impl Into<Arc<str>>) -> Self {
        Self {
            service,
            session_id: session_id.into(),
        }
    }

    /// Serves on any byte stream (an in-process `tokio::io::duplex` half,
    /// a pipe, a socket).
    pub async fn serve_io<S>(
        self,
        io: S,
    ) -> Result<RunningService<RoleServer, Self>, Box<rmcp::service::ServerInitializeError>>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        self.serve(io).await.map_err(Box::new)
    }

    /// Serves on this process's stdin and stdout until the client leaves.
    pub async fn serve_stdio(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let running = self.serve(rmcp::transport::stdio()).await?;
        running.waiting().await?;
        Ok(())
    }

    async fn call(&self, request: CallToolRequestParams, ct: tokio_util::sync::CancellationToken) -> Result<CallToolResult, ErrorData> {
        match request.name.as_ref() {
            REMOTE_BASH => {
                let args: BashArgs = parse(request.arguments)?;
                Ok(
                    match self.service.bash(&self.session_id, args.into(), &ct).await {
                        Ok(out) => {
                            let failed = out.exit_status != Some(0);
                            let mut result = tool_ok(&out);
                            if failed {
                                result.is_error = Some(true);
                            }
                            result
                        }
                        Err(e) => tool_error(&e),
                    },
                )
            }
            REMOTE_FILE => {
                let args: FileArgs = parse(request.arguments)?;
                let request = match args.into_request() {
                    Ok(r) => r,
                    Err(e) => return Err(ErrorData::invalid_params(e.to_string(), None)),
                };
                Ok(match self.service.file(&self.session_id, request, &ct).await {
                    Ok(out) => tool_ok(&out),
                    Err(e) => tool_error(&e),
                })
            }
            other => Err(ErrorData::invalid_params(
                Cow::Owned(format!("unknown tool `{other}`")),
                None,
            )),
        }
    }
}

impl ServerHandler for RemoteMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Work on SSH hosts that do not run the app. Host names come from the user's \
                 Remote hosts list.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.call(request, context.ct).await.map(Into::into)
    }
}
