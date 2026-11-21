//! RFB (VNC) client mapped to a pixel surface.
//!
//! [`RfbServer`] is the `vm` surface server. Each configured [`RfbEndpoint`]
//! is the surface `surface://vm/<name>`; opening it connects an RFB 3.8
//! client (RFC 6143) that keeps a local copy of the remote framebuffer,
//! publishes every framebuffer update as an RGBA [`loa_contracts::surface::Frame`] and
//! turns surface input into RFB pointer and key events. A dropped connection
//! is re-established with exponential backoff.

mod decode;
mod input;
mod keysym;
mod proto;
mod server;
mod surface;

#[cfg(all(test, feature = "testkit"))]
mod tests;

pub use decode::Cursor;
pub use server::{RfbEndpoint, RfbOptions, RfbServer, SERVER_NAME};
pub use surface::{ConnectionState, RfbSurface};

use async_trait::async_trait;
use loa_contracts::surface::SurfaceError;

/// Errors of the RFB client.
#[derive(Debug, thiserror::Error)]
pub enum RfbError {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("connecting to {0} timed out")]
    Timeout(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("the server refused the connection: {0}")]
    Refused(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("no password available for endpoint {0}")]
    NoPassword(String),
    #[error("no supported security type among {0:?}")]
    Security(Vec<u8>),
    #[error("zlib: {0}")]
    Zlib(#[from] flate2::DecompressError),
    #[error("no X11 keysym for key {0:?}")]
    Key(String),
    #[error("RFB pointer events have no forward button")]
    ForwardButton,
    #[error("not connected, reconnecting")]
    Disconnected,
}

pub type Result<T, E = RfbError> = std::result::Result<T, E>;

impl From<RfbError> for SurfaceError {
    fn from(e: RfbError) -> Self {
        SurfaceError::Backend(e.to_string())
    }
}

/// Supplies the VNC password of an endpoint each time a server asks for VNC
/// Authentication. The crate uses the password for one handshake and drops
/// it; it never stores it.
#[async_trait]
pub trait PasswordSource: Send + Sync {
    /// The password for the endpoint named `endpoint`, or `None` when there
    /// is none, which fails the handshake.
    async fn password(&self, endpoint: &str) -> Option<String>;
}

#[async_trait]
impl<F> PasswordSource for F
where
    F: Fn(&str) -> Option<String> + Send + Sync,
{
    async fn password(&self, endpoint: &str) -> Option<String> {
        self(endpoint)
    }
}
