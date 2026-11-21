use loa_contracts::surface::{SurfaceError, SurfaceUri};

/// Everything that can go wrong driving Chrome or a virtual display.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("no Chrome or Chromium executable found")]
    NoBrowser,
    #[error("browser configuration: {0}")]
    Config(String),
    #[error("chrome devtools protocol: {0}")]
    Cdp(#[from] chromiumoxide::error::CdpError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("virtual display: {0}")]
    Display(String),
    #[error("x11: {0}")]
    X11(String),
    #[error("page script: {0}")]
    Script(String),
    #[error("no tab {0}")]
    NotFound(SurfaceUri),
    #[error("the browser is shut down")]
    Closed,
    #[error(transparent)]
    Surface(#[from] SurfaceError),
}

pub type Result<T, E = BrowserError> = std::result::Result<T, E>;

impl From<BrowserError> for SurfaceError {
    fn from(error: BrowserError) -> Self {
        match error {
            BrowserError::NotFound(uri) => SurfaceError::NotFound(uri),
            BrowserError::Closed => SurfaceError::Closed,
            BrowserError::Surface(error) => error,
            other => SurfaceError::Backend(other.to_string()),
        }
    }
}

#[cfg(target_os = "linux")]
impl From<x11rb::errors::ConnectError> for BrowserError {
    fn from(error: x11rb::errors::ConnectError) -> Self {
        BrowserError::X11(error.to_string())
    }
}

#[cfg(target_os = "linux")]
impl From<x11rb::errors::ConnectionError> for BrowserError {
    fn from(error: x11rb::errors::ConnectionError) -> Self {
        BrowserError::X11(error.to_string())
    }
}

#[cfg(target_os = "linux")]
impl From<x11rb::errors::ReplyError> for BrowserError {
    fn from(error: x11rb::errors::ReplyError) -> Self {
        BrowserError::X11(error.to_string())
    }
}
