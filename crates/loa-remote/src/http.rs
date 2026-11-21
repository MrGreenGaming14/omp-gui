//! The `remote` tools on loopback streamable HTTP for harnesses that connect to MCP servers
//! themselves. Each session gets its own bearer token, which names the session its calls carry.

use loa_util::LockExt;
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use loa_contracts::harness::{McpServerSpec, McpTransport};
use loa_contracts::tools::{CallCtx, ToolSet};
use loa_mcp::ToolSetMcp;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::tools::SERVER_NAME;

type Tokens = Arc<Mutex<HashMap<String, String>>>;

const INSTRUCTIONS: &str = "Work on SSH hosts that do not run the app. Host names come from the user's Remote hosts list.";

/// The running HTTP server.
pub struct RemoteHttp {
    url: String,
    tokens: Tokens,
    stop: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for RemoteHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteHttp").field("url", &self.url).finish()
    }
}

async fn authenticate(State(tokens): State<Tokens>, mut request: Request, next: Next) -> Response {
    let bearer = request.headers().get(http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    match bearer.and_then(|token| tokens.locked().get(token).cloned()) {
        Some(session_id) => {
            request.extensions_mut().insert(CallCtx { session_id });
            next.run(request).await
        }
        None => http::StatusCode::UNAUTHORIZED.into_response(),
    }
}

impl RemoteHttp {
    /// Serves `tools` on `127.0.0.1` at a free port.
    pub async fn start(tools: ToolSet) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
        let addr = listener.local_addr()?;
        let (stop, tokens) = (CancellationToken::new(), Tokens::default());
        let mut config = StreamableHttpServerConfig::default();
        config.cancellation_token = stop.child_token();
        let server = move || Ok(ToolSetMcp::new(tools.clone(), SERVER_NAME).with_instructions(INSTRUCTIONS));
        let mcp = StreamableHttpService::new(server, Arc::new(LocalSessionManager::default()), config);
        let app = axum::Router::new().nest_service("/mcp", mcp).layer(axum::middleware::from_fn_with_state(tokens.clone(), authenticate));
        let shutdown = stop.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown.cancelled_owned()).await {
                tracing::warn!("remote MCP HTTP server stopped: {e}");
            }
        });
        Ok(Self { url: format!("http://{addr}/mcp"), tokens, stop, task: Some(task) })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The `remote` server spec for one session with a new bearer token; older tokens stay valid until [`RemoteHttp::forget`].
    pub fn session_spec(&self, session_id: &str) -> std::io::Result<McpServerSpec> {
        let token = loa_util::text::random_hex(32);
        self.tokens.locked().insert(token.clone(), session_id.to_owned());
        let headers = BTreeMap::from([("Authorization".to_owned(), format!("Bearer {token}"))]);
        Ok(McpServerSpec { name: SERVER_NAME.to_owned(), transport: McpTransport::Http { url: self.url.clone(), headers } })
    }

    /// Revokes every token of a session (it ended).
    pub fn forget(&self, session_id: &str) {
        self.tokens.locked().retain(|_, session| session != session_id);
    }

    /// Stops accepting requests and waits for the server task.
    pub async fn shutdown(mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for RemoteHttp {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
