//! Release watching, staging, gate checks, swap and rollback.

pub mod decision;
pub mod gates;
pub mod model;
pub mod peers;
pub mod state;
#[cfg(feature = "service")]
pub mod feeds;
#[cfg(feature = "service")]
pub mod service;
#[cfg(feature = "service")]
pub mod signature;
#[cfg(feature = "service")]
pub mod stage;
#[cfg(feature = "service")]
pub mod swap;
#[cfg(feature = "service")]
pub mod unpack;

pub use model::*;
#[cfg(feature = "service")]
pub use service::{TurnGate, Updater, UpdaterConfig, default_products};

use semver::Version;
use std::io;
use std::path::{Path, PathBuf};

/// Every error of the updater.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, #[source] source: io::Error },
    #[error("{} is not valid: {source}", path.display())]
    Json { path: PathBuf, #[source] source: serde_json::Error },
    #[error("unknown product {0}")]
    UnknownProduct(String),
    #[error("{product} is not installed: {reason}")]
    NotInstalled { product: String, reason: String },
    #[error("no update is offered for {0}; check its feed first")]
    NothingOffered(String),
    #[error("{product} {version} is not staged yet")]
    NotStaged { product: String, version: Version },
    #[error("cannot find the executable of {product} {version}: {reason}")]
    NoExecutable { product: String, version: Version, reason: String },
    #[error("request to {url} failed: {reason}")]
    Http { url: String, reason: String },
    #[error("{url} answered HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("rate limited until unix time {0}")]
    RateLimited(u64),
    #[error("feed: {0}")]
    Feed(String),
    #[error("{product} {version} has no {what}; refusing to install it")]
    Unverifiable { product: String, version: Version, what: &'static str },
    #[error("{0:?} is not a single file name")]
    BadName(String),
    #[error("{url} sent more than {limit} bytes")]
    TooLarge { url: String, limit: u64 },
    #[error("artifact {what} mismatch: expected {expected}, got {actual}")]
    Mismatch { what: &'static str, expected: String, actual: String },
    #[error("archive entry {entry:?} rejected: {reason}")]
    UnsafeEntry { entry: String, reason: &'static str },
    #[error("archive: {0}")]
    Archive(String),
    #[error("{0} has no previous version to roll back to")]
    NoPrevious(String),
    #[error("signature check failed: {} has no signature", .0.display())]
    Unsigned(PathBuf),
    #[error("signature check failed: {0}")]
    Signature(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Maps an i/o error to [`Error::Io`] at `path`.
pub(crate) fn io_at(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io { path: path.to_owned(), source }
}

/// Reads JSON at `path`; a missing file is `None`.
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let Some(bytes) = loa_util::fs::read_optional(path).map_err(io_at(path))? else { return Ok(None) };
    serde_json::from_slice(&bytes).map(Some).map_err(|source| Error::Json { path: path.to_owned(), source })
}

/// Replaces `path` with `value` as JSON, atomically.
pub(crate) fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    loa_util::fs::write_json_atomic(path, value).map_err(io_at(path))
}
