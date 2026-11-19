//! Ports the app implements: host key decisions (#97) and approvals (#100).

use async_trait::async_trait;

use crate::audit::Action;
use crate::known_hosts::KnownKey;

/// Why a host key needs a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyChange {
    /// No key is on file for this host.
    New,
    /// Other keys are on file: possible man in the middle.
    Changed { previous: Vec<KnownKey> },
}

/// What the decision card shows for an unknown or changed host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyPrompt {
    /// The configured host the connection is for.
    pub host: String,
    /// The hop presenting the key (the host itself or a ProxyJump hop).
    pub hostname: String,
    pub port: u16,
    pub key_type: String,
    /// `SHA256:…`
    pub fingerprint: String,
    pub change: HostKeyChange,
}

/// Decides whether to trust a host key. The app raises a decision card.
///
/// Returning `true` adds the key to `known_hosts` and connects; `false`
/// aborts the connection.
#[async_trait]
pub trait HostKeyDecider: Send + Sync {
    async fn decide(&self, prompt: HostKeyPrompt) -> bool;
}

/// A write or command waiting for the user's approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub session_id: String,
    pub host: String,
    pub action: Action,
    /// Path or command.
    pub target: String,
    /// Extra context for the card: byte count, edit preview, working dir.
    pub detail: String,
}

/// The user's answer to an [`ApprovalRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    AllowOnce,
    /// Allow this action kind on this host for the rest of the session.
    AllowSession,
    Deny { reason: Option<String> },
}

impl Approval {
    /// Maps a decision answer (`loa_core::answer`) to an approval.
    pub fn from_answer(answer: &str, reason: Option<String>) -> Option<Self> {
        use loa_core::answer;
        match answer {
            answer::ALLOW_ONCE => Some(Approval::AllowOnce),
            answer::ALLOW_SESSION => Some(Approval::AllowSession),
            answer::DENY => Some(Approval::Deny { reason }),
            _ => None,
        }
    }
}

/// Asks the user to approve a write or command. The app raises an approval card.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: ApprovalRequest) -> Approval;
}
