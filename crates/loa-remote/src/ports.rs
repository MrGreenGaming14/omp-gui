//! Ports the app implements: host key decisions (#97) and approvals (#100),
//! and [`PromptQueue`], the implementation of both that parks each question
//! until the user answers its decision card.

use loa_util::LockExt;
use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::{oneshot, watch};

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

/// A question waiting in a [`PromptQueue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    HostKey(HostKeyPrompt),
    Approval(ApprovalRequest),
}

impl Prompt {
    /// One line for the card header.
    pub fn title(&self) -> String {
        match self {
            Prompt::HostKey(p) => match p.change {
                HostKeyChange::New => format!("Trust the host key of {}?", p.host),
                HostKeyChange::Changed { .. } => format!("The host key of {} CHANGED", p.host),
            },
            Prompt::Approval(r) => match r.action {
                Action::Command => format!("Run a command on {}?", r.host),
                Action::Write => format!("Write a file on {}?", r.host),
                Action::Edit => format!("Edit a file on {}?", r.host),
                other => format!("Allow {} on {}?", other.as_str(), r.host),
            },
        }
    }

    /// The card body: what is asked, with the fingerprint or the target.
    pub fn body(&self) -> String {
        match self {
            Prompt::HostKey(p) => {
                let hop = crate::known_hosts::host_name(&p.hostname, p.port);
                let mut body = format!("{hop} presented {} key\n{}", p.key_type, p.fingerprint);
                match &p.change {
                    HostKeyChange::New => body.push_str("\nNo key is on file for it yet."),
                    HostKeyChange::Changed { previous } => {
                        body.push_str("\nThis may be a man in the middle. Keys on file:");
                        for key in previous {
                            body.push_str(&format!(
                                "\n{} {} ({}:{})",
                                key.key_type,
                                key.fingerprint,
                                key.file.display(),
                                key.line
                            ));
                        }
                    }
                }
                body
            }
            Prompt::Approval(r) if r.detail.is_empty() => r.target.clone(),
            Prompt::Approval(r) => format!("{}\n{}", r.target, r.detail),
        }
    }

    /// Session the question belongs to; host keys belong to none.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Prompt::HostKey(_) => None,
            Prompt::Approval(r) => Some(&r.session_id),
        }
    }
}

/// A parked question and the id to answer it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPrompt {
    pub id: u64,
    pub prompt: Prompt,
}

enum Reply {
    HostKey(oneshot::Sender<bool>),
    Approval(oneshot::Sender<Approval>),
}

#[derive(Default)]
struct Queue {
    next: u64,
    pending: BTreeMap<u64, (Prompt, Reply)>,
}

/// [`HostKeyDecider`] and [`Approver`] for the app: every question waits
/// here until [`PromptQueue::answer_host_key`] or
/// [`PromptQueue::answer_approval`] is called with its id. The UI follows
/// [`PromptQueue::subscribe`] and shows [`PromptQueue::pending`] as cards.
///
/// A question whose caller gives up (the tool call was cancelled) leaves
/// the queue by itself.
pub struct PromptQueue {
    queue: Mutex<Queue>,
    changed: watch::Sender<u64>,
}

impl Default for PromptQueue {
    fn default() -> Self {
        Self { queue: Mutex::default(), changed: watch::Sender::new(0) }
    }
}

impl std::fmt::Debug for PromptQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PromptQueue").field("pending", &self.queue.locked().pending.len()).finish()
    }
}

/// Takes a parked question out of the queue when its caller stops waiting.
struct Parked<'a> {
    queue: &'a PromptQueue,
    id: u64,
}

impl Drop for Parked<'_> {
    fn drop(&mut self) {
        if self.queue.queue.locked().pending.remove(&self.id).is_some() {
            self.queue.bump();
        }
    }
}

impl PromptQueue {
    pub fn new() -> Self {
        Self::default()
    }

    fn bump(&self) {
        self.changed.send_modify(|generation| *generation += 1);
    }

    fn park(&self, prompt: Prompt, reply: Reply) -> Parked<'_> {
        let id = {
            let mut queue = self.queue.locked();
            queue.next += 1;
            let id = queue.next;
            queue.pending.insert(id, (prompt, reply));
            id
        };
        self.bump();
        Parked { queue: self, id }
    }

    fn take(&self, id: u64) -> Option<Reply> {
        let reply = self.queue.locked().pending.remove(&id).map(|(_, reply)| reply);
        if reply.is_some() {
            self.bump();
        }
        reply
    }

    /// Ticks whenever a question arrives or leaves.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// The waiting questions, oldest first.
    pub fn pending(&self) -> Vec<PendingPrompt> {
        self.queue.locked()
            .pending
            .iter()
            .map(|(id, (prompt, _))| PendingPrompt { id: *id, prompt: prompt.clone() })
            .collect()
    }

    /// Answers a host key question. `false` when `id` isn't a waiting host key question.
    pub fn answer_host_key(&self, id: u64, accept: bool) -> bool {
        let mut queue = self.queue.locked();
        if !matches!(queue.pending.get(&id), Some((_, Reply::HostKey(_)))) {
            return false;
        }
        let Some((_, Reply::HostKey(reply))) = queue.pending.remove(&id) else {
            return false;
        };
        drop(queue);
        self.bump();
        reply.send(accept).is_ok()
    }

    /// Answers an approval. `false` when `id` isn't a waiting approval.
    pub fn answer_approval(&self, id: u64, approval: Approval) -> bool {
        let mut queue = self.queue.locked();
        if !matches!(queue.pending.get(&id), Some((_, Reply::Approval(_)))) {
            return false;
        }
        let Some((_, Reply::Approval(reply))) = queue.pending.remove(&id) else {
            return false;
        };
        drop(queue);
        self.bump();
        reply.send(approval).is_ok()
    }

    /// Denies every approval a session is still waiting on (it ended).
    pub fn deny_session(&self, session_id: &str) {
        let ids: Vec<u64> = self
            .queue
            .locked()
            .pending
            .iter()
            .filter(|(_, (prompt, _))| prompt.session_id() == Some(session_id))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(Reply::Approval(reply)) = self.take(id) {
                let _ = reply.send(Approval::Deny { reason: Some("the session ended".into()) });
            }
        }
    }
}

#[async_trait]
impl HostKeyDecider for PromptQueue {
    async fn decide(&self, prompt: HostKeyPrompt) -> bool {
        let (tx, rx) = oneshot::channel();
        let _parked = self.park(Prompt::HostKey(prompt), Reply::HostKey(tx));
        rx.await.unwrap_or(false)
    }
}

#[async_trait]
impl Approver for PromptQueue {
    async fn approve(&self, request: ApprovalRequest) -> Approval {
        let (tx, rx) = oneshot::channel();
        let _parked = self.park(Prompt::Approval(request), Reply::Approval(tx));
        rx.await.unwrap_or(Approval::Deny { reason: Some("no answer".into()) })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn request(session: &str) -> ApprovalRequest {
        ApprovalRequest {
            session_id: session.into(),
            host: "box".into(),
            action: Action::Command,
            target: "uptime".into(),
            detail: "timeout 120s".into(),
        }
    }

    async fn first(queue: &PromptQueue) -> PendingPrompt {
        let mut changes = queue.subscribe();
        loop {
            if let Some(p) = queue.pending().into_iter().next() {
                return p;
            }
            changes.changed().await.unwrap();
        }
    }

    #[tokio::test]
    async fn questions_wait_for_their_answer_by_id() {
        let queue = Arc::new(PromptQueue::new());
        let asking = tokio::spawn({
            let queue = queue.clone();
            async move { queue.approve(request("s1")).await }
        });
        let pending = first(&queue).await;
        assert_eq!(pending.prompt.title(), "Run a command on box?");
        assert_eq!(pending.prompt.body(), "uptime\ntimeout 120s");
        assert!(!queue.answer_host_key(pending.id, true), "wrong kind");
        assert!(queue.answer_approval(pending.id, Approval::AllowSession));
        assert_eq!(asking.await.unwrap(), Approval::AllowSession);
        assert!(queue.pending().is_empty());
        assert!(!queue.answer_approval(pending.id, Approval::AllowOnce), "answered once");

        let deciding = tokio::spawn({
            let queue = queue.clone();
            async move {
                queue
                    .decide(HostKeyPrompt {
                        host: "box".into(),
                        hostname: "10.0.0.1".into(),
                        port: 2222,
                        key_type: "ssh-ed25519".into(),
                        fingerprint: "SHA256:abc".into(),
                        change: HostKeyChange::New,
                    })
                    .await
            }
        });
        let pending = first(&queue).await;
        assert_eq!(pending.prompt.title(), "Trust the host key of box?");
        assert!(pending.prompt.body().starts_with("[10.0.0.1]:2222 presented ssh-ed25519 key\nSHA256:abc"));
        assert!(queue.answer_host_key(pending.id, true));
        assert!(deciding.await.unwrap());
    }

    #[tokio::test]
    async fn cancelled_callers_and_ended_sessions_leave_the_queue() {
        let queue = Arc::new(PromptQueue::new());
        let asking = tokio::spawn({
            let queue = queue.clone();
            async move { queue.approve(request("s1")).await }
        });
        first(&queue).await;
        asking.abort();
        let _ = asking.await;
        assert!(queue.pending().is_empty(), "an aborted call takes its card away");

        let asking = tokio::spawn({
            let queue = queue.clone();
            async move { queue.approve(request("s2")).await }
        });
        first(&queue).await;
        queue.deny_session("other");
        assert_eq!(queue.pending().len(), 1);
        queue.deny_session("s2");
        assert!(matches!(asking.await.unwrap(), Approval::Deny { .. }));
        assert!(queue.pending().is_empty());
    }
}
