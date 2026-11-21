use crate::registry::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("`{0}` isn't a valid server name: use letters, digits, `-`, `_` or `.`")]
    InvalidName(String),
    #[error("the command line is empty")]
    EmptyCommand,
    #[error("the command line has an unclosed quote")]
    UnclosedQuote,
    #[error("`{url}` isn't a usable MCP url: {reason}")]
    Url { url: String, reason: String },
    #[error("`{0}` isn't a valid HTTP header")]
    Header(String),
    #[error("the `{0}` header is set by the MCP client and can't be overridden")]
    ReservedHeader(String),
    #[error("no server named `{0}`")]
    UnknownServer(String),
    #[error("a server named `{0}` already exists")]
    NameTaken(String),
    #[error("server `{0}` isn't running")]
    NotRunning(String),
    #[error("tool `{tool}` of server `{server}` isn't allowed here")]
    NotAllowed { server: String, tool: String },
    #[error("couldn't connect to `{server}`: {message}")]
    Connect { server: String, message: String },
    #[error("server `{server}`: {source}")]
    Service { server: String, source: Box<rmcp::ServiceError> },
    #[error("server config store: {0}")]
    Store(#[source] StoreError),
    #[error("building the HTTP client: {0}")]
    HttpClient(String),
    #[error("omp bridge: {0}")]
    Bridge(String),
}

impl Error {
    /// Maps an rmcp error of `server`.
    pub(crate) fn service(server: &str) -> impl FnOnce(rmcp::ServiceError) -> Self {
        move |source| Self::Service { server: server.to_owned(), source: Box::new(source) }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
