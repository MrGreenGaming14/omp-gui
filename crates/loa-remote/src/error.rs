use std::path::PathBuf;

/// Everything that can go wrong in the remote server.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("resolving {host}: {message}")]
    Resolve { host: String, message: String },
    #[error("host `{0}` is not configured")]
    UnknownHost(String),
    #[error("ProxyJump chain for `{0}` loops back on itself")]
    ProxyJumpCycle(String),
    #[error("ProxyJump chain for `{0}` is deeper than {max} hops", max = crate::config::MAX_JUMPS)]
    ProxyJumpTooDeep(String),
    #[error("no ssh-agent socket for {host}: set IdentityAgent or SSH_AUTH_SOCK")]
    NoAgent { host: String },
    #[error("ssh-agent at {path}: {message}")]
    Agent { path: PathBuf, message: String },
    #[error("authentication as {user}@{host} failed: none of the {tried} agent identities was accepted")]
    AuthFailed { user: String, host: String, tried: usize },
    #[error("host key for {host} refused: {reason}")]
    HostKeyRejected { host: String, reason: String },
    #[error("ssh to {host}: {source}")]
    Ssh { host: String, source: russh::Error },
    #[error("connecting to {host}: {source}")]
    Connect { host: String, source: std::io::Error },
    #[error("connecting to {host} timed out")]
    ConnectTimeout { host: String },
    #[error("sftp on {host}: {message}")]
    Sftp { host: String, message: String },
    #[error("known_hosts {path}: {message}")]
    KnownHosts { path: PathBuf, message: String },
    #[error("refused by host limits: {0}")]
    Refused(String),
    #[error("denied by the user{}", .0.as_deref().map(|r| format!(": {r}")).unwrap_or_default())]
    Denied(Option<String>),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("file {path} is {size} bytes, over the {max} byte limit")]
    TooLarge { path: String, size: u64, max: u64 },
    #[error("edit of {path}: {message}")]
    Edit { path: String, message: String },
    #[error("audit log: {0}")]
    Audit(#[from] rusqlite::Error),
    #[error("cancelled")]
    Cancelled,
    #[error("timed out")]
    TimedOut,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
