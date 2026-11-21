//! Ports the app implements, host key decisions and approvals, and [`PromptQueue`], which
//! implements both by parking each question until the user answers it by id.

use loa_util::LockExt;
use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::{oneshot, watch};

use crate::audit::Action;
use crate::known_hosts::KnownKey;

/// Why a host key needs a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyChange {
    New,
    /// Other keys are on file: possible man in the middle.
    Changed { previous: Vec<KnownKey> },
}

/// An unknown or changed host key, presented by `hostname:port` while connecting to `host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyPrompt {
    pub host: String,
    pub hostname: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
    pub change: HostKeyChange,
}

/// Decides whether to trust a host key; `true` saves it to `known_hosts` and connects.
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
    /// Byte count, edit preview or working directory.
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
        match answer {
            loa_core::answer::ALLOW_ONCE => Some(Approval::AllowOnce),
            loa_core::answer::ALLOW_SESSION => Some(Approval::AllowSession),
            loa_core::answer::DENY => Some(Approval::Deny { reason }),
            _ => None,
        }
    }
}

/// Asks the user to approve a write or command.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: ApprovalRequest) -> Approval;
}

/// A question waiting in a [`PromptQueue`]: a host key answered with `bool`, an approval with [`Approval`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    HostKey(HostKeyPrompt),
    Approval(ApprovalRequest),
}

impl Prompt {
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
pub struct PromptView {
    pub id: u64,
    pub prompt: Prompt,
}

#[derive(Default)]
struct Queue {
    next: u64,
    pending: BTreeMap<u64, (Prompt, Box<dyn Any + Send>)>,
}

/// [`HostKeyDecider`] and [`Approver`] for the app: questions wait until [`PromptQueue::answer`];
/// a question whose caller gives up leaves by itself.
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
struct Parked<'a>(&'a PromptQueue, u64);

impl Drop for Parked<'_> {
    fn drop(&mut self) {
        if self.0.queue.locked().pending.remove(&self.1).is_some() {
            self.0.bump();
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

    async fn ask<R: Send + 'static>(&self, prompt: Prompt) -> Option<R> {
        let (tx, rx) = oneshot::channel::<R>();
        let id = {
            let mut queue = self.queue.locked();
            queue.next += 1;
            let id = queue.next;
            queue.pending.insert(id, (prompt, Box::new(tx)));
            id
        };
        self.bump();
        let _parked = Parked(self, id);
        rx.await.ok()
    }

    /// Ticks whenever a question arrives or leaves.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// The waiting questions, oldest first.
    pub fn pending(&self) -> Vec<PromptView> {
        self.queue.locked().pending.iter().map(|(id, (prompt, _))| PromptView { id: *id, prompt: prompt.clone() }).collect()
    }

    /// Answers question `id`; `false` when it isn't waiting or takes another reply type.
    pub fn answer<R: Send + 'static>(&self, id: u64, reply: R) -> bool {
        let mut queue = self.queue.locked();
        if !queue.pending.get(&id).is_some_and(|(_, tx)| tx.is::<oneshot::Sender<R>>()) {
            return false;
        }
        let tx = queue.pending.remove(&id).and_then(|(_, tx)| tx.downcast::<oneshot::Sender<R>>().ok());
        drop(queue);
        self.bump();
        tx.is_some_and(|tx| tx.send(reply).is_ok())
    }

    /// Denies every approval a session is still waiting on (it ended).
    pub fn deny_session(&self, session_id: &str) {
        for view in self.pending().into_iter().filter(|v| v.prompt.session_id() == Some(session_id)) {
            self.answer(view.id, Approval::Deny { reason: Some("the session ended".into()) });
        }
    }
}

#[async_trait]
impl HostKeyDecider for PromptQueue {
    async fn decide(&self, prompt: HostKeyPrompt) -> bool {
        self.ask(Prompt::HostKey(prompt)).await.unwrap_or(false)
    }
}

#[async_trait]
impl Approver for PromptQueue {
    async fn approve(&self, request: ApprovalRequest) -> Approval {
        self.ask(Prompt::Approval(request)).await.unwrap_or(Approval::Deny { reason: Some("no answer".into()) })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn request(session: &str) -> ApprovalRequest {
        let (target, detail) = ("uptime".into(), "timeout 120s".into());
        ApprovalRequest { session_id: session.into(), host: "box".into(), action: Action::Command, target, detail }
    }

    async fn first(queue: &PromptQueue) -> PromptView {
        let mut changes = queue.subscribe();
        loop {
            if let Some(p) = queue.pending().into_iter().next() {
                return p;
            }
            changes.changed().await.unwrap();
        }
    }

    #[tokio::test]
    async fn questions_wait_for_a_typed_answer_and_leave_when_abandoned() {
        let queue = Arc::new(PromptQueue::new());
        let ask = |session: &'static str| {
            let queue = queue.clone();
            tokio::spawn(async move { queue.approve(request(session)).await })
        };
        let asking = ask("s1");
        let pending = first(&queue).await;
        assert!(matches!(&pending.prompt, Prompt::Approval(r) if r.target == "uptime"));
        assert!(!queue.answer(pending.id, true), "wrong reply type");
        assert!(queue.answer(pending.id, Approval::AllowSession));
        assert_eq!(asking.await.unwrap(), Approval::AllowSession);
        assert!(!queue.answer(pending.id, Approval::AllowOnce), "answered once");

        let deciding = tokio::spawn({
            let queue = queue.clone();
            let (hostname, key_type, fingerprint) = ("10.0.0.1".into(), "ssh-ed25519".into(), "SHA256:abc".into());
            async move { queue.decide(HostKeyPrompt { host: "box".into(), hostname, port: 2222, key_type, fingerprint, change: HostKeyChange::New }).await }
        });
        assert!(queue.answer(first(&queue).await.id, true));
        assert!(deciding.await.unwrap());

        let asking = ask("s1");
        first(&queue).await;
        asking.abort();
        let _ = asking.await;
        assert!(queue.pending().is_empty(), "an aborted call takes its question away");
        let asking = ask("s2");
        first(&queue).await;
        queue.deny_session("other");
        assert_eq!(queue.pending().len(), 1);
        queue.deny_session("s2");
        assert!(matches!(asking.await.unwrap(), Approval::Deny { .. }));
        assert!(queue.pending().is_empty());
    }
}
