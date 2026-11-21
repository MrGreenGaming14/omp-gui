//! The `remote` MCP server on loopback streamable HTTP, for harnesses that
//! connect to MCP servers themselves (Claude Code, Codex, opencode, ACP).
//!
//! One server serves every session. Each session gets its own bearer token;
//! the token names the session the audit rows and approval cards carry, and
//! a request without a known token never reaches a tool.

use loa_util::LockExt;
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use loa_adapter::{McpServerSpec, McpTransport};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::RemoteService;
use crate::mcp::{CallerSession, RemoteMcp, SERVER_NAME};

type Tokens = Arc<Mutex<HashMap<String, Arc<str>>>>;


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

fn token() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

async fn authenticate(State(tokens): State<Tokens>, mut request: Request, next: Next) -> Response {
    let session = request
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .and_then(|token| tokens.locked().get(token).cloned());
    match session {
        Some(session) => {
            request.extensions_mut().insert(CallerSession(session));
            next.run(request).await
        }
        None => http::StatusCode::UNAUTHORIZED.into_response(),
    }
}

impl RemoteHttp {
    /// Serves `service` on `127.0.0.1` at a free port.
    pub async fn start(service: Arc<RemoteService>) -> std::io::Result<Self> {
        Self::bind(service, SocketAddr::from(([127, 0, 0, 1], 0))).await
    }

    pub async fn bind(service: Arc<RemoteService>, addr: SocketAddr) -> std::io::Result<Self> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let stop = CancellationToken::new();
        let tokens: Tokens = Arc::default();
        let mut config = StreamableHttpServerConfig::default();
        config.cancellation_token = stop.child_token();
        let mcp = StreamableHttpService::new(
            move || Ok(RemoteMcp::per_request(service.clone())),
            Arc::new(LocalSessionManager::default()),
            config,
        );
        let app = axum::Router::new()
            .nest_service("/mcp", mcp)
            .layer(axum::middleware::from_fn_with_state(tokens.clone(), authenticate));
        let shutdown = stop.clone();
        let task = tokio::spawn(async move {
            let served = axum::serve(listener, app).with_graceful_shutdown(shutdown.cancelled_owned()).await;
            if let Err(e) = served {
                tracing::warn!("remote MCP HTTP server stopped: {e}");
            }
        });
        Ok(Self { url: format!("http://{addr}/mcp"), tokens, stop, task: Some(task) })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// The `remote` server spec for one session: the URL and the session's
    /// own bearer token. Calling it again for the same session issues a new
    /// token and keeps the old one valid until [`RemoteHttp::forget`].
    pub fn session_spec(&self, session_id: &str) -> std::io::Result<McpServerSpec> {
        let token = token()?;
        self.tokens.locked().insert(token.clone(), Arc::from(session_id));
        Ok(McpServerSpec {
            name: SERVER_NAME.to_owned(),
            transport: McpTransport::Http {
                url: self.url.clone(),
                headers: BTreeMap::from([("Authorization".to_owned(), format!("Bearer {token}"))]),
            },
        })
    }

    /// Revokes every token of a session (it ended).
    pub fn forget(&self, session_id: &str) {
        self.tokens.locked().retain(|_, session| &**session != session_id);
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
