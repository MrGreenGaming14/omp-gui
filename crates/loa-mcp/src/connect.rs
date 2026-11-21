//! Opening one client connection to a server spec.

use std::collections::{HashMap, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStderr;
use tokio::task::JoinHandle;

use crate::spec::{HttpSpec, ServerSpec, StdioSpec, Transport};
use crate::{Error, Result};

/// Stderr lines kept for error messages.
const STDERR_LINES: usize = 20;

/// How long a crashed child may keep its stderr open before its last words
/// are given up on (a grandchild can inherit the pipe).
const STDERR_DRAIN: Duration = Duration::from_secs(1);

pub(crate) struct Connection<H: ClientHandler> {
    pub service: RunningService<RoleClient, H>,
    pub stderr: Option<StderrTail>,
}

/// The tail of a child's stderr, collected in the background.
pub(crate) struct StderrTail {
    lines: Arc<Mutex<VecDeque<String>>>,
    task: JoinHandle<()>,
}

impl StderrTail {
    fn spawn(server: &str, stderr: ChildStderr) -> Self {
        let lines = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_LINES)));
        let sink = lines.clone();
        let server = server.to_owned();
        let task = tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                tracing::debug!(server, "stderr: {line}");
                let mut lines = sink.lock().unwrap_or_else(PoisonError::into_inner);
                if lines.len() == STDERR_LINES {
                    lines.pop_front();
                }
                lines.push_back(line);
            }
        });
        Self { lines, task }
    }

    /// The last non empty line the child wrote, once its stderr is closed.
    pub async fn last_line(self) -> Option<String> {
        let abort = self.task.abort_handle();
        if tokio::time::timeout(STDERR_DRAIN, self.task).await.is_err() {
            abort.abort();
        }
        let lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        lines.iter().rev().find(|line| !line.trim().is_empty()).cloned()
    }
}

/// Starts the server (or opens the HTTP session) and runs the MCP handshake.
pub(crate) async fn connect<H: ClientHandler>(
    spec: &ServerSpec,
    handler: H,
    http: &reqwest::Client,
    timeout: Duration,
) -> Result<Connection<H>> {
    let failed = |message: String| Error::Connect { server: spec.name.clone(), message };
    match &spec.transport {
        Transport::Stdio(stdio) => {
            let (transport, stderr) = spawn(stdio).map_err(|e| failed(format!("can't run `{}`: {e}", stdio.command)))?;
            let stderr = stderr.map(|stderr| StderrTail::spawn(&spec.name, stderr));
            match tokio::time::timeout(timeout, handler.serve(transport)).await {
                Ok(Ok(service)) => Ok(Connection { service, stderr }),
                Ok(Err(e)) => {
                    let last = match stderr {
                        Some(stderr) => stderr.last_line().await,
                        None => None,
                    };
                    Err(failed(match last {
                        Some(last) => format!("{e}: {last}"),
                        None => e.to_string(),
                    }))
                }
                Err(_) => Err(failed(format!("no handshake within {timeout:?}"))),
            }
        }
        Transport::Http(spec_http) => {
            let transport = http_transport(spec_http, http).map_err(failed)?;
            match tokio::time::timeout(timeout, handler.serve(transport)).await {
                Ok(Ok(service)) => Ok(Connection { service, stderr: None }),
                Ok(Err(e)) => Err(failed(e.to_string())),
                Err(_) => Err(failed(format!("no handshake within {timeout:?}"))),
            }
        }
    }
}

fn spawn(stdio: &StdioSpec) -> std::io::Result<(TokioChildProcess, Option<ChildStderr>)> {
    let path_var = stdio.env.iter().find(|(k, _)| env_key_is_path(k)).map(|(_, v)| v);
    let program = loa_exec::resolve_in(std::path::Path::new(&stdio.command), path_var.map(std::ffi::OsStr::new));
    let mut command = tokio::process::Command::new(program);
    command.args(&stdio.args).envs(&stdio.env);
    if let Some(cwd) = &stdio.cwd {
        #[cfg(windows)]
        command.current_dir(loa_exec::simplify(cwd));
        #[cfg(not(windows))]
        command.current_dir(cwd);
    }
    #[cfg_attr(not(any(unix, windows)), expect(unused_mut))]
    let mut command = process_wrap::tokio::CommandWrap::from(command);
    // Its own process group, so shutdown also reaches what the server
    // spawned (`npx` runs the real server as a grandchild).
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::tokio::KillOnDrop).wrap(process_wrap::tokio::JobObject);
    TokioChildProcess::builder(command).stderr(Stdio::piped()).spawn()
}

/// Whether `key` names `PATH`: case-insensitive on Windows, exact elsewhere.
fn env_key_is_path(key: &str) -> bool {
    if cfg!(windows) { key.eq_ignore_ascii_case("PATH") } else { key == "PATH" }
}

fn http_transport(
    spec: &HttpSpec,
    http: &reqwest::Client,
) -> Result<StreamableHttpClientTransport<reqwest::Client>, String> {
    let mut headers = HashMap::with_capacity(spec.headers.len());
    for (name, value) in &spec.headers {
        let name = http::HeaderName::from_bytes(name.as_bytes()).map_err(|e| format!("header `{name}`: {e}"))?;
        let value = http::HeaderValue::from_str(value).map_err(|e| format!("header `{name}`: {e}"))?;
        headers.insert(name, value);
    }
    let config = StreamableHttpClientTransportConfig::with_uri(spec.url.as_str()).custom_headers(headers);
    Ok(StreamableHttpClientTransport::with_client(http.clone(), config))
}

/// The HTTP client every streamable HTTP server shares: rustls on ring with
/// the Mozilla root store, no redirects (a redirect would replay the
/// configured headers to wherever it points).
pub(crate) fn http_client() -> Result<reqwest::Client> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::HttpClient(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .pool_max_idle_per_host(0)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| Error::HttpClient(e.to_string()))
}
