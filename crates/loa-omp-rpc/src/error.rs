//! Errors of the RPC client.

use std::process::ExitStatus;

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// The `omp` executable couldn't be started because it doesn't exist.
    #[error("the omp executable `{0}` wasn't found")]
    NotFound(String),
    #[error("omp i/o failed: {0}")]
    Io(#[from] std::io::Error),
    /// omp exited, or closed its stdout, while the client still needed it.
    /// `stderr` holds the tail of what it wrote to stderr.
    #[error("omp exited ({}){}", status.map_or_else(|| "unknown status".to_owned(), |s| s.to_string()), if stderr.is_empty() { String::new() } else { format!(": {stderr}") })]
    Exited { status: Option<ExitStatus>, stderr: String },
    /// omp speaks no protocol version this client supports.
    #[error("omp offers RPC protocol versions {offered:?}, this client supports {min}..={max}")]
    Version { offered: Vec<u32>, min: u32, max: u32 },
    /// omp sent something that breaks the protocol.
    #[error("RPC protocol error: {0}")]
    Protocol(String),
    /// A command came back with `success: false`.
    #[error("omp rejected `{command}`: {message}")]
    Command { command: String, message: String, code: Option<String> },
    /// omp didn't send its `ready` frame in time.
    #[error("omp didn't become ready within {0:?}")]
    StartTimeout(std::time::Duration),
    /// The client was shut down.
    #[error("the RPC client is closed")]
    Closed,
    #[error("malformed JSON from omp: {0}")]
    Json(#[from] serde_json::Error),
}

impl RpcError {
    /// The machine readable code of a failed command (`session_busy`, `stale_cursor`, …).
    pub fn code(&self) -> Option<&str> {
        match self {
            RpcError::Command { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

pub type Result<T, E = RpcError> = std::result::Result<T, E>;
