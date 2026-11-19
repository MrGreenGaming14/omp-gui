/// Everything that can go wrong while bridging omp's tools to MCP.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The omp program could not be started.
    #[error("could not start `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    /// Reading or writing omp's pipes, or preparing the bridge's state directory, failed.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// omp closed its stdout. `stderr` holds the last lines it wrote there.
    #[error("omp exited: {stderr}")]
    OmpExited { stderr: String },
    /// omp wrote a frame the RPC protocol does not allow.
    #[error("malformed RPC frame from omp: {0}")]
    Frame(String),
    /// omp answered a command with `success: false`.
    #[error("omp refused `{command}`: {message}")]
    Rejected { command: String, message: String },
    /// The tool is not one this bridge offers.
    #[error("unknown tool `{0}`")]
    UnknownTool(String),
    /// omp ended the turn without executing the requested tool.
    #[error("omp ended the turn without running `{tool}`: {reason}")]
    NotRun { tool: String, reason: String },
    /// The MCP client cancelled the call.
    #[error("tool call cancelled")]
    Cancelled,
    /// The MCP transport failed.
    #[error("MCP transport error: {0}")]
    Mcp(String),
    /// JSON (de)serialization failed.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Result with the bridge's [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;
