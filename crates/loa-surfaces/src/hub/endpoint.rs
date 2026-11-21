//! The built-in surface servers as streamable HTTP MCP servers on loopback
//! (`http://127.0.0.1:<port>/<server>`), for harnesses that connect to MCP
//! servers themselves. Each declares its surfaces as `surface://` resources
//! and serves the surface tools; requests must carry the bearer token.

use crate::hub::service::SurfaceHub;
use crate::hub::tools::{SurfaceTools, tool_set};
use base64::Engine;
use loa_contracts::harness::{McpServerSpec, McpTransport};
use loa_contracts::surface::{SurfaceMode, SurfaceUri};
use loa_mcp::ToolSetMcp;
use rmcp::RoleServer;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, ErrorData, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ServerCapabilities,
    ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::transport::StreamableHttpService;
use rmcp::transport::streamable_http_server::StreamableHttpServerConfig;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::task::JoinHandle;

/// MCP server names are `surface-<server>`, e.g. `surface-browser`.
pub const SERVER_PREFIX: &str = "surface-";

const SERVERS: [&str; 3] = [crate::browser::SERVER_NAME, crate::docs::SERVER_NAME, crate::vnc::SERVER_NAME];

/// One built-in surface server: its scoped [`ToolSetMcp`] plus its surfaces as resources.
#[derive(Clone)]
struct Scoped {
    tools: ToolSetMcp,
    hub: Arc<SurfaceHub>,
    scope: &'static str,
}

impl ServerHandler for Scoped {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
    }

    async fn list_tools(&self, r: Option<PaginatedRequestParams>, c: RequestContext<RoleServer>) -> Result<ListToolsResult, ErrorData> {
        self.tools.list_tools(r, c).await
    }

    async fn call_tool(&self, r: CallToolRequestParams, c: RequestContext<RoleServer>) -> Result<CallToolResponse, ErrorData> {
        self.tools.call_tool(r, c).await
    }

    async fn list_resources(&self, _: Option<PaginatedRequestParams>, _: RequestContext<RoleServer>) -> Result<ListResourcesResult, ErrorData> {
        let listed = self.hub.list().await;
        let ours = listed.iter().filter(|l| l.info.uri.server() == self.scope);
        let resources = ours.filter_map(|l| serde_json::from_value::<Resource>(crate::hub::mcp::resource(&l.info)).ok());
        Ok(ListResourcesResult::with_all_items(resources.collect()))
    }

    /// A document surface's file, or a pixel surface's current picture as PNG.
    async fn read_resource(&self, request: ReadResourceRequestParams, _: RequestContext<RoleServer>) -> Result<ReadResourceResponse, ErrorData> {
        let not_found = || ErrorData::resource_not_found(request.uri.clone(), None);
        let uri: SurfaceUri = request.uri.parse().map_err(|_| not_found())?;
        if uri.server() != self.scope {
            return Err(not_found());
        }
        let opened = self.hub.open(&uri).await.map_err(|_| not_found())?;
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
            let hub = tools.hub().clone();
            let handler = Scoped { tools: ToolSetMcp::new(tool_set(&hub, Some(scope)), &format!("{SERVER_PREFIX}{scope}")), hub, scope };
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
