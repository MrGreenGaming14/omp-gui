//! The single writer of a live session.

use loa_util::LockExt;
use crate::queue::{Policy, Queue, Subscription};
use crate::{NodeKey, SessionId};
use loa_core::{CommitError, Dom, Draft, Patch, PatchSink};
use std::sync::{Arc, Mutex, PoisonError};

pub type PersistError = Box<dyn std::error::Error + Send + Sync>;

/// Durable storage for a session's patches, normally its journal. A port:
/// `loa-app` backs it with `loa-journal`.
pub trait Persist: Send {
    /// Stores `patch`. Called in sequence order, after the patch was checked
    /// against the DOM and before it is applied or published. An error fails
    /// the commit and leaves the session unchanged.
    fn append(&mut self, patch: &Patch) -> Result<(), PersistError>;

    /// Called with the DOM right after the patch [`Persist::append`] stored
    /// was applied, still in sequence order and under the writer's lock.
    /// Storage writes snapshots here. The patch is already durable, so this
    /// can't fail the commit.
    fn applied(&mut self, dom: &Dom) {
        let _ = dom;
    }
}

/// Why a peer's request was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Denied(pub String);

/// Decides whether a peer may make a change to a session this node owns. A
/// port: `loa-app` backs it with `loa-trust`.
pub trait Authorize: Send + Sync {
    /// Called at the point the request is ordered, with the DOM it would apply
    /// to, so subtree scoped grants see the current tree.
    fn authorize(&self, session: &SessionId, requester: &NodeKey, draft: &Draft, dom: &Dom) -> Result<(), Denied>;
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum RequestError {
    #[error("request denied: {0}")]
    Denied(#[from] Denied),
    #[error("stale session revision: expected {expected}, actual {actual}")]
    Conflict { expected: u64, actual: u64 },
    #[error(transparent)]
    Commit(#[from] CommitError),
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ReplicateError {
    /// This node orders the session itself and takes no patches from others.
    #[error("this node owns the session")]
    Owner,
    #[error(transparent)]
    Commit(#[from] CommitError),
}

/// Everything a [`SessionWriter`] starts from.
pub struct WriterConfig {
    pub session: SessionId,
    /// Key of the node that orders this session's patches.
    pub owner: NodeKey,
    /// Key of this node.
    pub local: NodeKey,
    /// The session as persisted so far (a new DOM, or the journal replayed).
    pub dom: Dom,
    pub persist: Box<dyn Persist>,
    pub authorize: Arc<dyn Authorize>,
}

/// The [`PatchSink`] of one live session.
///
/// On the owner node it orders local drafts ([`PatchSink::commit_at`]) and
/// peer requests ([`SessionWriter::request`]) into one gap free sequence, in
/// the order they reach it. On any other node it only mirrors the owner's
/// patches ([`SessionWriter::replicate`]). Each patch is checked, persisted,
/// applied and then published to subscribers, all under one lock, so every
/// subscriber sees the same order. Publishing never waits on a subscriber.
pub struct SessionWriter {
    session: SessionId,
    owner: NodeKey,
    local: NodeKey,
    authorize: Arc<dyn Authorize>,
    inner: Mutex<Inner>,
}

struct Inner {
    dom: Dom,
    persist: Box<dyn Persist>,
    subs: Vec<Arc<Queue>>,
    closed: bool,
}

impl SessionWriter {
    pub fn new(config: WriterConfig) -> Self {
        let WriterConfig { session, owner, local, dom, persist, authorize } = config;
        Self { session, owner, local, authorize, inner: Mutex::new(Inner { dom, persist, subs: Vec::new(), closed: false }) }
    }

    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn owner(&self) -> NodeKey {
        self.owner
    }

    /// Whether this node orders the session's patches.
    pub fn is_writer(&self) -> bool {
        self.owner == self.local
    }

    /// Sequence of the last committed patch.
    pub fn seq(&self) -> u64 {
        self.inner.locked().dom.seq()
    }

    /// Reads the current DOM without copying it. Commits wait while `read` runs.
    pub fn with_dom<R>(&self, read: impl FnOnce(&Dom) -> R) -> R {
        read(&self.inner.locked().dom)
    }

    /// The current DOM and a subscription that starts with the patch right
    /// after it.
    pub fn subscribe(&self, policy: Policy) -> (Dom, Subscription) {
        let mut inner = self.inner.locked();
        let sub = Self::attach(&mut inner, policy);
        (inner.dom.clone(), sub)
    }

    /// The current sequence and a subscription that starts with the patch
    /// right after it. For subscribers that read the journal up to that
    /// sequence instead of copying the DOM.
    pub fn subscribe_seq(&self, policy: Policy) -> (u64, Subscription) {
        let mut inner = self.inner.locked();
        let sub = Self::attach(&mut inner, policy);
        (inner.dom.seq(), sub)
    }

    fn attach(inner: &mut Inner, policy: Policy) -> Subscription {
        let queue = Queue::new(policy);
        if inner.closed {
            queue.close();
        } else {
            inner.subs.push(Arc::clone(&queue));
        }
        Subscription::new(queue)
    }

    /// Orders a change requested by peer `requester`, after [`Authorize`]
    /// allows it. Requests and local commits are ordered as they arrive.
    pub fn request(&self, requester: &NodeKey, draft: Draft, ts: i64) -> Result<Patch, RequestError> {
        let mut inner = self.inner.locked();
        self.writable(&inner)?;
        self.authorize.authorize(&self.session, requester, &draft, &inner.dom)?;
        let patch = inner.dom.prepare(draft, ts);
        Ok(self.write(&mut inner, patch)?)
    }

    /// Orders a peer change only if the owner still has the revision the peer
    /// saw. The revision check and grant check run under the writer lock; a
    /// local commit cannot slip between them and the write.
    pub fn request_if_seq(
        &self,
        requester: &NodeKey,
        expected_seq: u64,
        draft: Draft,
        ts: i64,
    ) -> Result<Patch, RequestError> {
        let mut inner = self.inner.locked();
        self.writable(&inner)?;
        let actual = inner.dom.seq();
        if actual != expected_seq {
            return Err(RequestError::Conflict { expected: expected_seq, actual });
        }
        self.authorize.authorize(&self.session, requester, &draft, &inner.dom)?;
        let patch = inner.dom.prepare(draft, ts);
        Ok(self.write(&mut inner, patch)?)
    }

    /// Mirrors a patch the owner ordered. Only on nodes that don't own the
    /// session; patches must arrive in sequence.
    pub fn replicate(&self, patch: Patch) -> Result<(), ReplicateError> {
        if self.is_writer() {
            return Err(ReplicateError::Owner);
        }
        let mut inner = self.inner.locked();
        if inner.closed {
            return Err(CommitError::Closed.into());
        }
        self.write(&mut inner, patch)?;
        Ok(())
    }

    /// Ends the session: later commits fail with [`CommitError::Closed`] and
    /// subscriptions end after what they already hold.
    pub fn close(&self) {
        Self::shut(&mut self.inner.locked());
    }

    fn shut(inner: &mut Inner) {
        inner.closed = true;
        for queue in inner.subs.drain(..) {
            queue.close();
        }
    }

    fn writable(&self, inner: &Inner) -> Result<(), CommitError> {
        if inner.closed {
            Err(CommitError::Closed)
        } else if !self.is_writer() {
            Err(CommitError::NotWriter)
        } else {
            Ok(())
        }
    }

    /// Checks, persists, applies and publishes the next patch.
    fn write(&self, inner: &mut Inner, patch: Patch) -> Result<Patch, CommitError> {
        inner.dom.check(&patch)?;
        inner.persist.append(&patch).map_err(|e| CommitError::Journal(e.to_string()))?;
        // Already checked against this DOM, so this cannot fail.
        inner.dom.apply(&patch)?;
        let Inner { dom, persist, .. } = inner;
        persist.applied(dom);
        Ok(self.publish(inner, patch))
    }

    fn publish(&self, inner: &mut Inner, patch: Patch) -> Patch {
        if inner.subs.is_empty() {
            return patch;
        }
        let shared = Arc::new(patch);
        inner.subs.retain(|queue| queue.push(&shared));
        Arc::try_unwrap(shared).unwrap_or_else(|shared| Patch::clone(&shared))
    }
}

impl PatchSink for SessionWriter {
    fn commit_at(&self, draft: Draft, ts: i64) -> Result<Patch, CommitError> {
        let mut inner = self.inner.locked();
        self.writable(&inner)?;
        let patch = inner.dom.prepare(draft, ts);
        self.write(&mut inner, patch)
    }
}

impl Drop for SessionWriter {
    fn drop(&mut self) {
        Self::shut(self.inner.get_mut().unwrap_or_else(PoisonError::into_inner));
    }
}

#[cfg(feature = "testkit")]
mod testkit {
    use super::*;
    use loa_core::testkit::{AllowAll, DenyAll, MemoryPersist};

    impl Authorize for AllowAll {
        fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
            Ok(())
        }
    }

    impl Authorize for DenyAll {
        fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
            Err(Denied("denied by test".into()))
        }
    }

    impl Persist for MemoryPersist {
        fn append(&mut self, patch: &Patch) -> Result<(), PersistError> {
            if let Some(why) = &self.fail {
                return Err(why.clone().into());
            }
            self.patches.locked().push(patch.clone());
            Ok(())
        }
    }
}
