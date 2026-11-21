//! The built-in surface servers as MCP servers on loopback, for harnesses
//! that connect to MCP servers themselves (Claude Code, Codex, opencode,
//! ACP agents).
//!
//! Each built-in server (`browser`, `docs`, `vm`) is its own streamable HTTP
//! MCP server at `http://127.0.0.1:<port>/<server>`. It declares its surfaces
//! as `surface://` resources and serves the surface tools for them. Requests
//! must carry the endpoint's bearer token, so other local processes can't
//! drive the user's browser.

use crate::tools::{SurfaceTools, ToolContent, ToolResult, tool_definitions};
use loa_contracts::harness::{McpServerSpec, McpTransport};
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData, JsonObject, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ServerCapabilities, ServerConfig, Tool,
};
use loa_contracts::surface::SurfaceMode;
use rmcp::service::RequestContext;
use rmcp::RoleServer;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::StreamableHttpServerConfig;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// MCP server names are `surface-<server>`, e.g. `surface-browser`.
pub const SERVER_PREFIX: &str = "surface-";

const SERVERS: [&str; 3] = [loa_surface_browser::SERVER_NAME, loa_surface_docs::SERVER_NAME, loa_surface_rfb::SERVER_NAME];

/// One built-in surface server as an MCP server.
#[derive(Clone)]
struct Scoped {
    tools: Arc<SurfaceTools>,
    scope: &'static str,
}

fn tool(definition: &Value) -> Option<Tool> {
    let name = definition["name"].as_str()?.to_owned();
    let description = definition["description"].as_str().unwrap_or_default().to_owned();
    let schema: JsonObject = serde_json::from_value(definition["inputSchema"].clone()).ok()?;
    Some(Tool::new(name, description, schema))
}

fn content(result: ToolResult) -> CallToolResult {
    use base64::Engine;
    let blocks = result
        .content
        .into_iter()
        .map(|block| match block {
            ToolContent::Text(text) => ContentBlock::text(text),
            ToolContent::Image { mime, data } => {
                ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(&data), mime)
            }
        })
        .collect();
    if result.is_error { CallToolResult::error(blocks) } else { CallToolResult::success(blocks) }
}

impl ServerHandler for Scoped {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let tools = tool_definitions(Some(self.scope)).iter().filter_map(tool).collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = Value::Object(request.arguments.unwrap_or_default());
        Ok(content(self.tools.run(&request.name, &args, Some(self.scope)).await).into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let listed = self.tools.hub().list().await;
        let resources = listed
            .iter()
            .filter(|l| l.info.uri.server() == self.scope)
            .filter_map(|l| serde_json::from_value::<Resource>(loa_surface_proto::mcp::resource(&l.info)).ok())
            .collect();
        Ok(ListResourcesResult::with_all_items(resources))
    }

    /// A document surface's file, or a pixel surface's current picture as PNG.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        use base64::Engine;
        let not_found = || ErrorData::resource_not_found(request.uri.clone(), None);
        let uri: loa_contracts::surface::SurfaceUri = request.uri.parse().map_err(|_| not_found())?;
        if uri.server() != self.scope {
            return Err(not_found());
        }
        let opened = self.tools.hub().open(&uri).await.map_err(|_| not_found())?;
        let failed = |e: loa_contracts::surface::SurfaceError| ErrorData::internal_error(e.to_string(), None);
        let (mime, data) = match opened.info.mode {
            SurfaceMode::Document => {
                let document = opened.surface.document().await.map_err(failed)?;
                (document.mime, document.data)
            }
            SurfaceMode::Pixel => ("image/png".to_owned(), opened.surface.screenshot().await.map_err(failed)?),
        };
        let blob = base64::engine::general_purpose::STANDARD.encode(&data);
        let contents = ResourceContents::BlobResourceContents { uri: request.uri, mime_type: Some(mime), blob, meta: None };
        Ok(ReadResourceResult::new(vec![contents]).into())
    }
}

/// The running loopback MCP endpoint. Dropping it stops the server.
pub struct McpEndpoint {
    base: String,
    token: String,
    task: JoinHandle<()>,
}

impl Drop for McpEndpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl McpEndpoint {
    /// Serves `tools` on `127.0.0.1` at a free port.
    pub async fn start(tools: Arc<SurfaceTools>) -> std::io::Result<Self> {
        let token = loa_util::text::random_hex(24);
        let mut router = axum::Router::new();
        for scope in SERVERS {
            let handler = Scoped { tools: tools.clone(), scope };
            let service: StreamableHttpService<Scoped, LocalSessionManager> = StreamableHttpService::new(
                move || Ok(handler.clone()),
                Arc::new(LocalSessionManager::default()),
                StreamableHttpServerConfig::default(),
            );
            router = router.nest_service(&format!("/{scope}"), service);
        }
        let expected = format!("Bearer {token}");
        let check = move |request: axum::extract::Request, next: axum::middleware::Next| {
            let allowed = request.headers().get("authorization").is_some_and(|v| v.as_bytes() == expected.as_bytes());
            async move {
                if allowed {
                    next.run(request).await
                } else {
                    axum::response::IntoResponse::into_response(axum::http::StatusCode::UNAUTHORIZED)
                }
            }
        };
        let router = router.layer(axum::middleware::from_fn(check));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::warn!(%error, "surface MCP endpoint stopped");
            }
        });
        Ok(Self { base, token, task })
    }

    /// `http://127.0.0.1:<port>/<server>` of a built-in server.
    pub fn url(&self, server: &str) -> String {
        format!("{}/{server}", self.base)
    }

    /// The bearer token every request must carry.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The built-in servers as MCP servers for a session config.
    pub fn specs(&self) -> Vec<McpServerSpec> {
        SERVERS
            .iter()
            .map(|server| McpServerSpec {
                name: format!("{SERVER_PREFIX}{server}"),
                transport: McpTransport::Http {
                    url: self.url(server),
                    headers: BTreeMap::from([("Authorization".to_owned(), format!("Bearer {}", self.token))]),
                },
            })
            .collect()
    }
}
