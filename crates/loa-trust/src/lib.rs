//! Node identity and the Biscuit grants bound to it.
//!
//! One ed25519 key is the node's network address (the iroh endpoint id), its
//! grant root and its holder key. [`NodeIdentity`] owns the secret, [`PublicKey`]
//! is what peers see, and a [`KeyStore`] keeps the seed between runs.
//!
//! A grant gives one peer (the holder) rights on this node's resources. The
//! [`KINDS`] table describes every [`Resource`] kind once; the token rules,
//! right facts, narrowing checks and scope decoding are generated from it.
//!
//! # Token layout
//!
//! The authority block, signed by the issuer, holds the rights as facts
//! (`session_right`, `prompt_right`, `project_right`, `future_right`,
//! `ssh_right`, `surface_right`, `sandbox_right`, `folder_right`) and an
//! expiry check on `time_ms`. Every re-share appends a third party block signed
//! by the previous holder's key that holds only checks: the narrowed scopes and
//! its own expiry, so a re-share can narrow but never widen. Each block's
//! context records the grant id, its holder, its expiry and whether that holder
//! may re-share. Time is always unix milliseconds.

mod key;
mod keystore;
mod renew;
mod scope;
mod store;
mod token;

#[cfg(test)]
mod tests;

pub use key::{Challenge, NodeIdentity, Proof, PublicKey, SECRET_KEY_LENGTH, prove};
pub use keystore::{FallbackStore, FileKeyStore, KeySource, KeyStore, MemoryKeyStore, OsKeychain, SecretBytes, load_app_identity};
pub use renew::{RenewFailure, RenewOutcome, RenewTransport, Renewer};
pub use scope::{Action, KINDS, Request, Resource, ResourceKind, Scope, SessionFacts, Target, turn_of};
pub use store::{GrantSpec, GrantStore, IssuedGrant, IssuedRecord, Narrowing, ReceivedGrant};
pub use token::{Denied, GrantId, Link, Presented, Revocations, VerifiedGrant, Verifier, token_scopes};

/// Name of the app's MCP server for SSH tools.
pub const REMOTE_SERVER: &str = "remote";

/// Default lifetime of a token. Grants renew well before this runs out.
pub const DEFAULT_TTL_MS: i64 = 10 * 60_000;

/// Everything that can go wrong with keys and grants. No variant carries
/// secret bytes, so errors are safe to log.
#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("key store: {0}")]
    Store(String),
    /// The OS credential store can't be used at all; a [`FallbackStore`] moves on.
    #[error("os keychain unavailable: {0}")]
    Unavailable(String),
    /// The store holds a value that is not a 32 byte seed. It is left in place:
    /// a new key would change the node's address.
    #[error("stored node key has {found} bytes, expected {SECRET_KEY_LENGTH}")]
    WrongLength { found: usize },
    #[error("os random source: {0}")]
    Random(String),
    #[error("invalid key: {0}")]
    Key(String),
    #[error("grant database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("grant encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error("biscuit: {0}")]
    Biscuit(#[from] biscuit_auth::error::Token),
    #[error("{resource} grants don't support `{action}`")]
    Action { resource: &'static str, action: Action },
    #[error("a grant needs at least one scope")]
    NoScopes,
    #[error("a scope needs at least one action")]
    NoActions,
    #[error("token lifetime must be positive, got {0} ms")]
    Ttl(i64),
    #[error("node id {0} doesn't fit a grant")]
    NodeId(loa_core::NodeId),
    #[error("a folder grant needs a normalized absolute path, got {0:?}")]
    FolderPath(String),
    #[error("unknown grant {0}")]
    UnknownGrant(GrantId),
    #[error("grant {0} is revoked")]
    Revoked(GrantId),
    #[error("grant {0} has ended")]
    Expired(GrantId),
    #[error("the requester doesn't hold grant {0}")]
    NotHolder(GrantId),
    #[error("grant {0} may not be re-shared")]
    ReshareForbidden(GrantId),
    #[error("the grant {0} this one was re-shared from has no current token")]
    ParentExpired(GrantId),
    #[error("malformed grant token: {0}")]
    Malformed(String),
}

pub type Result<T, E = TrustError> = std::result::Result<T, E>;
