//! Opening one client connection to a server spec.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use loa_util::exec::StderrTail;
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};

use crate::spec::{ServerSpec, Transport};
use crate::{Error, Result};

/// How long a crashed child may keep stderr open (a grandchild can inherit it) before its last words are given up on.
const STDERR_DRAIN: Duration = Duration::from_secs(1);

pub(crate) struct Connection<H: ClientHandler> {
    pub service: RunningService<RoleClient, H>,
    pub stderr: Option<StderrTail>,
}

/// The last non empty line a child wrote to stderr, once the stream closed.
pub(crate) async fn last_line(stderr: Option<StderrTail>) -> Option<String> {
    let stderr = stderr?;
    let _ = tokio::time::timeout(STDERR_DRAIN, stderr.closed()).await;
    stderr.joined().lines().rev().find(|line| !line.trim().is_empty()).map(str::to_owned)
}

/// `stderr` passed through unchanged, each line also logged at debug level.
fn logged(server: &str, stderr: impl AsyncRead + Unpin + Send + 'static) -> impl AsyncRead + Unpin + Send + 'static {
    let (mut tx, rx) = tokio::io::duplex(8 << 10);
    let server = server.to_owned();
    tokio::spawn(async move {
        let (mut reader, mut line) = (BufReader::new(stderr), Vec::new());
        while reader.read_until(b'\n', &mut line).await.is_ok_and(|n| n > 0) {
            tracing::debug!(server, "stderr: {}", String::from_utf8_lossy(&line).trim_end());
            if tx.write_all(&line).await.is_err() {
                break;
            }
            line.clear();
        }
    });
    rx
}

/// Starts the server (or opens the HTTP session) and runs the MCP handshake.
pub(crate) async fn connect<H: ClientHandler>(spec: &ServerSpec, handler: H, http: &reqwest::Client, timeout: Duration) -> Result<Connection<H>> {
    let failed = |message: String| Error::Connect { server: spec.name.clone(), message };
    let (serving, stderr) = match &spec.transport {
        Transport::Stdio(stdio) => {
            let mut command = tokio::process::Command::new(&stdio.command);
            command.args(&stdio.args).envs(&stdio.env);
            if let Some(cwd) = &stdio.cwd {
                command.current_dir(cwd);
            }
            #[cfg_attr(not(unix), expect(unused_mut))]
            let mut command = process_wrap::tokio::CommandWrap::from(command);
            // Its own process group, so shutdown also reaches what the server spawned (`npx` runs a grandchild).
            #[cfg(unix)]
            command.wrap(process_wrap::tokio::ProcessGroup::leader());
            let (transport, stderr) = TokioChildProcess::builder(command)
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| failed(format!("can't run `{}`: {e}", stdio.command)))?;
            let stderr = stderr.map(|s| StderrTail::spawn(logged(&spec.name, s), 20));
            (tokio::time::timeout(timeout, handler.serve(transport)).await, stderr)
        }
        Transport::Http(spec) => {
            let mut headers = HashMap::with_capacity(spec.headers.len());
            for (name, value) in &spec.headers {
                let bad = |e: &dyn std::fmt::Display| failed(format!("header `{name}`: {e}"));
                headers.insert(http::HeaderName::from_bytes(name.as_bytes()).map_err(|e| bad(&e))?, http::HeaderValue::from_str(value).map_err(|e| bad(&e))?);
            }
            let config = StreamableHttpClientTransportConfig::with_uri(spec.url.as_str()).custom_headers(headers);
            let transport = StreamableHttpClientTransport::with_client(http.clone(), config);
            (tokio::time::timeout(timeout, handler.serve(transport)).await, None)
        }
    };
    match serving {
        Ok(Ok(service)) => Ok(Connection { service, stderr }),
        Ok(Err(e)) => Err(failed(match last_line(stderr).await {
            Some(last) => format!("{e}: {last}"),
            None => e.to_string(),
        })),
        Err(_) => Err(failed(format!("no handshake within {timeout:?}"))),
    }
}

/// The client every streamable HTTP server shares: rustls on ring with the
/// Mozilla roots, no redirects (one would replay the configured headers).
pub(crate) fn http_client() -> Result<reqwest::Client> {
    let failed = |e: &dyn std::fmt::Display| Error::HttpClient(e.to_string());
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| failed(&e))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let builder = reqwest::Client::builder().tls_backend_preconfigured(tls).pool_max_idle_per_host(0);
    builder.redirect(reqwest::redirect::Policy::none()).build().map_err(|e| failed(&e))
}
