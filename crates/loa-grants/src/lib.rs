//! Biscuit grants bound to node keys.
//!
//! A grant gives one peer (the holder) rights on this node's resources:
//! sessions, single prompts, projects, every future session, SSH hosts and
//! surfaces. It travels as a Biscuit token signed by the issuing node's
//! ed25519 key.
//!
//! - [`Scope`], [`Resource`], [`Action`]: what a grant covers
//! - [`GrantStore`]: SQLite tables of grants issued, grants received and
//!   revocations; issuing, renewal, revocation and offline re-sharing
//! - [`Verifier`], [`VerifiedGrant`]: holder proof, chain checks and
//!   per-request authorization
//! - [`Renewer`] and the [`RenewTransport`] port: the holder side refreshing
//!   short lived tokens before they expire
//!
//! # Token layout
//!
//! The authority block, signed by the issuer, holds the rights as facts
//! (`session_right`, `prompt_right`, `project_right`, `future_right`,
//! `ssh_right`, `surface_right`) and an expiry check on `now_ms`. Every
//! re-share appends a third party block signed by the previous holder's key
//! that holds only checks: the narrowed scopes and its own expiry. Facts in
//! appended blocks are never trusted by the authorizer, so a re-share can
//! narrow but never widen.
//!
//! Each block's context string records the grant id, the holder it was issued
//! to, its expiry and whether that holder may re-share. The verifier walks
//! that chain: every appended block must be signed by the holder named in the
//! block before it, and the requester must be the last holder and prove it by
//! signing the verifier's challenge.
//!
//! Time is always passed in as unix milliseconds.

mod keys;
mod renew;
mod scope;
mod select;
mod store;
mod token;

#[cfg(test)]
mod tests;

pub use keys::{Challenge, NodeKey, NodePublicKey, Proof, prove};
pub use renew::{RenewFailure, RenewOutcome, RenewTransport, Renewer};
pub use scope::{Action, Request, Resource, Scope, SessionFacts, Target, turn_of};
pub use select::{REMOTE_SERVER, Tool, ToolAccess, ToolSource, Visibility};
pub use store::{GrantSpec, GrantStore, IssuedGrant, IssuedRecord, Narrowing, ReceivedGrant};
pub use token::{Denied, GrantId, Link, Presented, Revocations, VerifiedGrant, Verifier, token_scopes};

/// Default lifetime of a token. Grants renew well before this runs out.
pub const DEFAULT_TTL_MS: i64 = 10 * 60_000;

#[derive(Debug, thiserror::Error)]
pub enum GrantError {
    #[error("grant database: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("grant encoding: {0}")]
    Json(#[from] serde_json::Error),
    #[error("biscuit: {0}")]
    Biscuit(#[from] biscuit_auth::error::Token),
    #[error("invalid key: {0}")]
    Key(String),
    #[error("random source failed: {0}")]
    Random(String),
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

pub type Result<T, E = GrantError> = std::result::Result<T, E>;
