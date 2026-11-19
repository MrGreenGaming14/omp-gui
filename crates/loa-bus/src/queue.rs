//! Per subscriber bounded queues.
//!
//! The writer pushes into each queue without waiting: a full queue marks its
//! subscriber lagged and detaches it, so one slow reader never holds up the
//! writer or the other readers.

use crate::SessionId;
use loa_core::{ApplyError, Dom, NodeId, Op, Patch};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::sync::Notify;

/// How a subscriber is fed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    /// Undelivered events the subscriber may hold. One more marks it lagged.
    /// Values below 1 count as 1.
    pub capacity: usize,
    /// Merge consecutive `append` patches to the same node while they wait in
    /// the queue. For views that only need the latest text (the UI); never for
    /// subscribers that need every patch (peers, the journal).
    pub coalesce_appends: bool,
}

impl Policy {
    /// Every patch, one by one.
    pub const fn every_patch(capacity: usize) -> Self {
        Self { capacity, coalesce_appends: false }
    }

    /// Streaming appends to one node merged while the subscriber is behind.
    pub const fn coalescing(capacity: usize) -> Self {
        Self { capacity, coalesce_appends: true }
    }
}

/// Consecutive `append` patches to one node, merged for a coalescing
/// subscriber. Covers sequence numbers `first_seq..=last_seq`.
#[derive(Clone, Debug, PartialEq)]
pub struct Appends {
    pub node: NodeId,
    pub first_seq: u64,
    pub last_seq: u64,
    /// Timestamp of the last merged patch.
    pub ts: i64,
    /// The merged text, in order.
    pub text: String,
}

impl Appends {
    /// Applies the merged appends to a DOM mirror, which ends at `last_seq`.
    pub fn apply_to(self, dom: &mut Dom) -> Result<(), ApplyError> {
        let Appends { node, first_seq, last_seq, ts, text } = self;
        dom.apply(&Patch { seq: first_seq, node, ts, op: Op::Append { text } })?;
        // The remaining sequence numbers carry no text of their own: empty
        // appends advance the mirror's sequence without changing the node.
        for seq in first_seq + 1..=last_seq {
            dom.apply(&Patch { seq, node, ts, op: Op::Append { text: String::new() } })?;
        }
        Ok(())
    }
}

/// What a subscriber receives.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Patch(Arc<Patch>),
    /// Only for subscribers with [`Policy::coalesce_appends`].
    Appends(Appends),
    /// The subscriber fell behind and was detached. Everything before
    /// `resync_from` was delivered; it resubscribes or replays the journal
    /// from `resync_from`. Nothing follows this event.
    Lagged { resync_from: u64 },
}

impl Event {
    /// Applies a patch or merged appends to a DOM mirror. Returns `false` for
    /// [`Event::Lagged`], which carries nothing to apply.
    pub fn apply_to(self, dom: &mut Dom) -> Result<bool, ApplyError> {
        match self {
            Event::Patch(patch) => dom.apply(&patch).map(|()| true),
            Event::Appends(appends) => appends.apply_to(dom).map(|()| true),
            Event::Lagged { .. } => Ok(false),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TryRecvError {
    #[error("no event is waiting")]
    Empty,
    /// The session closed, or the subscriber lagged and got its notice.
    #[error("the subscription has ended")]
    Closed,
}

/// One subscriber's queue. `K` tags each event with its source: `()` for a
/// session subscription, the session id for the firehose.
pub(crate) struct Queue<K> {
    state: Mutex<State<K>>,
    notify: Notify,
    policy: Policy,
}

struct State<K> {
    items: VecDeque<(K, Event)>,
    /// Set when the queue overflowed: the source and sequence of the first
    /// patch not delivered.
    lagged: Option<(K, u64)>,
    closed: bool,
    receiver_gone: bool,
}

impl<K: Clone + PartialEq> Queue<K> {
    pub(crate) fn new(policy: Policy) -> Arc<Self> {
        let policy = Policy { capacity: policy.capacity.max(1), ..policy };
        Arc::new(Self {
            state: Mutex::new(State {
                items: VecDeque::with_capacity(policy.capacity.min(1024)),
                lagged: None,
                closed: false,
                receiver_gone: false,
            }),
            notify: Notify::new(),
            policy,
        })
    }

    fn lock(&self) -> MutexGuard<'_, State<K>> {
        // Every critical section leaves the state consistent before any call
        // that could panic, so a poisoned lock holds valid data.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Offers `patch` from `source`. Never waits. Returns `false` once the
    /// queue no longer takes patches (lagged, closed or receiver dropped), so
    /// the publisher can forget it.
    pub(crate) fn push(&self, source: &K, patch: &Arc<Patch>) -> bool {
        let mut state = self.lock();
        if state.receiver_gone || state.closed || state.lagged.is_some() {
            return false;
        }
        if self.policy.coalesce_appends
            && let Op::Append { text } = &patch.op
            && let Some((last_source, last)) = state.items.back_mut()
            && last_source == source
            && coalesce(last, patch.node, patch.seq, patch.ts, text)
        {
            // The receiver was already woken for the event this merged into.
            return true;
        }
        if state.items.len() >= self.policy.capacity {
            state.lagged = Some((source.clone(), patch.seq));
            drop(state);
            self.notify.notify_one();
            return false;
        }
        state.items.push_back((source.clone(), Event::Patch(Arc::clone(patch))));
        drop(state);
        self.notify.notify_one();
        true
    }

    /// Ends the subscription after what is already queued.
    pub(crate) fn close(&self) {
        self.lock().closed = true;
        self.notify.notify_one();
    }

    fn take(&self) -> Result<(K, Event), TryRecvError> {
        let mut state = self.lock();
        if let Some(item) = state.items.pop_front() {
            return Ok(item);
        }
        if let Some((source, resync_from)) = state.lagged.take() {
            state.closed = true;
            return Ok((source, Event::Lagged { resync_from }));
        }
        if state.closed { Err(TryRecvError::Closed) } else { Err(TryRecvError::Empty) }
    }

    fn len(&self) -> usize {
        self.lock().items.len()
    }
}

/// Merges an append into the last queued event when both act on `node` and
/// the sequence numbers are adjacent.
fn coalesce(last: &mut Event, node: NodeId, seq: u64, ts: i64, text: &str) -> bool {
    match last {
        Event::Patch(prev) if prev.node == node && prev.seq + 1 == seq => {
            let Op::Append { text: prev_text } = &prev.op else {
                return false;
            };
            let mut merged = String::with_capacity(prev_text.len() + text.len());
            merged.push_str(prev_text);
            merged.push_str(text);
            *last = Event::Appends(Appends { node, first_seq: prev.seq, last_seq: seq, ts, text: merged });
            true
        }
        Event::Appends(appends) if appends.node == node && appends.last_seq + 1 == seq => {
            appends.text.push_str(text);
            appends.last_seq = seq;
            appends.ts = ts;
            true
        }
        _ => false,
    }
}

/// The receiving end of a [`Queue`]. Dropping it detaches the subscriber.
struct Receiver<K: Clone + PartialEq> {
    queue: Arc<Queue<K>>,
}

impl<K: Clone + PartialEq> Receiver<K> {
    fn try_recv(&mut self) -> Result<(K, Event), TryRecvError> {
        self.queue.take()
    }

    async fn recv(&mut self) -> Option<(K, Event)> {
        loop {
            match self.queue.take() {
                Ok(item) => return Some(item),
                Err(TryRecvError::Closed) => return None,
                // A push between `take` and `notified` leaves a permit, so
                // this wait can't miss it.
                Err(TryRecvError::Empty) => self.queue.notify.notified().await,
            }
        }
    }
}

impl<K: Clone + PartialEq> Drop for Receiver<K> {
    fn drop(&mut self) {
        let mut state = self.queue.lock();
        state.receiver_gone = true;
        state.items.clear();
    }
}

/// A subscriber to one session. Works with any async runtime; `try_recv`
/// works without one.
pub struct Subscription {
    rx: Receiver<()>,
}

impl Subscription {
    pub(crate) fn new(queue: Arc<Queue<()>>) -> Self {
        Self { rx: Receiver { queue } }
    }

    /// The next event without waiting.
    pub fn try_recv(&mut self) -> Result<Event, TryRecvError> {
        self.rx.try_recv().map(|((), event)| event)
    }

    /// Waits for the next event. `None` once the session closed and
    /// everything queued was delivered, or after [`Event::Lagged`].
    pub async fn recv(&mut self) -> Option<Event> {
        self.rx.recv().await.map(|((), event)| event)
    }

    /// Events waiting in the queue.
    pub fn pending(&self) -> usize {
        self.rx.queue.len()
    }
}

/// A subscriber to every session on a [`crate::Bus`], for the sidebar. Each
/// event is tagged with its session. A lag notice names the session and
/// sequence of the first patch that was not delivered; the subscriber then
/// resubscribes and rereads every session it shows.
pub struct Firehose {
    rx: Receiver<SessionId>,
}

impl Firehose {
    pub(crate) fn new(queue: Arc<Queue<SessionId>>) -> Self {
        Self { rx: Receiver { queue } }
    }

    pub fn try_recv(&mut self) -> Result<(SessionId, Event), TryRecvError> {
        self.rx.try_recv()
    }

    /// Waits for the next event. `None` once the bus is gone or after a lag
    /// notice.
    pub async fn recv(&mut self) -> Option<(SessionId, Event)> {
        self.rx.recv().await
    }

    pub fn pending(&self) -> usize {
        self.rx.queue.len()
    }
}
