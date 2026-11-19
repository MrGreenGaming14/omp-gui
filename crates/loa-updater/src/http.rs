//! HTTP client shared by the release feeds and the stager.

use std::sync::Arc;
use std::time::Duration;

/// Errors building the HTTP client.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("TLS configuration: {0}")]
    Tls(#[from] rustls::Error),
    #[error("building HTTP client: {0}")]
    Client(#[from] reqwest::Error),
}

pub type Result<T, E = HttpError> = std::result::Result<T, E>;

/// User agent sent with every request; GitHub rejects requests without one.
const USER_AGENT: &str = concat!("loa-updater/", env!("CARGO_PKG_VERSION"));

/// How long establishing a connection may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// How long a response may stall between reads before the request fails.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// A client on rustls with the ring provider and the Mozilla root store.
/// Follows redirects (release downloads redirect to a CDN); plain `http`
/// URLs work too.
pub fn client() -> Result<reqwest::Client> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .user_agent(USER_AGENT)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()?)
}
