//! Live sessions. Each open session is a loa-bus [`SessionWriter`] whose
//! [`Persist`](loa_bus::Persist) port appends to its loa-journal journal, so
//! every patch an adapter commits is on disk before the UI sees it. The UI
//! keeps its own copy of each DOM, fed by the writer's subscription
//! ([`Sessions::watch`]); [`Sessions::feed`] applies what arrives and names
//! the [`SessionEvent`]s areas react to.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use iced::Subscription;
use iced::futures::{SinkExt, Stream};
use loa_adapter::HarnessSession;
use loa_bus::{Event, Policy, SessionId, SessionWriter};
use loa_core::{DecisionStatus, Dom, ElementKind, NodeId, Op, Patch, TurnStatus, attr};
use loa_features::library::{Library, Live};
use loa_journal::{JournalWriter, Listing};
use loa_ui_shell::SessionSummary;

use crate::services::Services;

/// Undelivered patches a session feed may hold before it resubscribes.
const FEED_CAPACITY: usize = 4096;

/// What happened to a session, as every area sees it through
/// [`Area::on_session_event`](crate::area::Area::on_session_event). When an
/// event arrives the session's DOM in `Ctx` already reflects it.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// This session became the one the chat shows; `None` when none is.
    Activated(Option<SessionId>),
    /// The session's DOM was replaced wholesale: started, opened from its
    /// journal, or resynced after its feed fell behind. Rebuild from the DOM.
    Loaded(SessionId),
    /// One patch was applied. Merged streaming appends arrive as one
    /// `append` patch carrying the merged text and the first merged `seq`.
    Patch(SessionId, Arc<Patch>),
    /// A `<turn>` was inserted.
    TurnStarted { session: SessionId, turn: NodeId },
    /// A turn ended `done`, `error` or `cancelled`.
    TurnEnded { session: SessionId, turn: NodeId, status: TurnStatus },
    /// A `<decision>` waits for an answer.
    DecisionWaiting { session: SessionId, decision: NodeId },
    /// A `<decision>` was answered.
    DecisionAnswered { session: SessionId, decision: NodeId },
    /// A `<compaction>` was inserted.
    Compaction { session: SessionId, node: NodeId },
    /// The session was closed and its harness stopped.
    Closed(SessionId),
}

/// What a session's feed delivers to the app.
#[derive(Clone, Debug)]
pub enum Feed {
    /// The writer's DOM when the feed (re)subscribed; events follow it.
    Synced(Arc<Dom>),
    Event(Event),
}

/// One open session.
pub struct Session {
    id: SessionId,
    writer: Arc<SessionWriter>,
    journal: Arc<JournalWriter>,
    /// The UI's copy of the writer's DOM.
    dom: Dom,
    /// The running harness; `None` until the session is started or resumed.
    harness: Option<Arc<dyn HarnessSession>>,
    busy: bool,
}

impl Session {
    fn new(id: SessionId, live: Live, dom: Dom) -> Self {
        let busy = dom_busy(&dom);
        Self { id, writer: live.writer, journal: live.journal, dom, harness: None, busy }
    }

    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// The session as the UI last saw it.
    pub fn dom(&self) -> &Dom {
        &self.dom
    }

    /// The session's single writer: the `PatchSink` adapters write through.
    pub fn writer(&self) -> &Arc<SessionWriter> {
        &self.writer
    }

    /// The session's journal, for forks, rewinds and reads of past states.
    pub fn journal(&self) -> &Arc<JournalWriter> {
        &self.journal
    }

    pub fn harness(&self) -> Option<&Arc<dyn HarnessSession>> {
        self.harness.as_ref()
    }

    /// Id of the harness the session runs on: the running one, else the
    /// `harness` attribute of its `<session>`.
    pub fn harness_id(&self) -> Option<&str> {
        self.harness.as_ref().map(|harness| harness.harness()).or_else(|| self.dom.root()?.str_attr(attr::HARNESS))
    }

    /// A turn runs or waits, or a prompt was sent and its turn hasn't started.
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    pub fn has_turns(&self) -> bool {
        dom_turns(&self.dom).next().is_some()
    }

    /// The sidebar entry of the session.
    pub fn summary(&self) -> Option<SessionSummary> {
        SessionSummary::from_dom(&self.dom, false)
    }

    pub(crate) fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    /// Attaches the harness that now drives the session and catches the
    /// DOM up with what it wrote while starting.
    pub(crate) fn attach(&mut self, harness: Arc<dyn HarnessSession>) {
        self.harness = Some(harness);
        self.dom = self.writer.with_dom(Dom::clone);
        self.busy = dom_busy(&self.dom);
    }

    /// Takes the running harness off the session; the next send resumes it
    /// on a fresh harness.
    pub(crate) fn detach(&mut self) -> Option<Arc<dyn HarnessSession>> {
        self.harness.take()
    }

    /// Applies one feed event. `Ok(None)` when it carries nothing new.
    fn apply(&mut self, event: Event) -> Result<Option<Arc<Patch>>, loa_core::ApplyError> {
        let seq = self.dom.seq();
        match event {
            Event::Patch(patch) if patch.seq <= seq => Ok(None),
            Event::Patch(patch) => {
                self.dom.apply(&patch)?;
                Ok(Some(patch))
            }
            Event::Appends(appends) if appends.last_seq <= seq => Ok(None),
            Event::Appends(appends) => {
                let patch = Patch {
                    seq: appends.first_seq,
                    node: appends.node,
                    ts: appends.ts,
                    op: Op::Append { text: appends.text },
                };
                self.dom.apply(&patch)?;
                // The other merged patches carry no text of their own.
                for seq in appends.first_seq + 1..=appends.last_seq {
                    let empty = Patch { seq, node: patch.node, ts: patch.ts, op: Op::Append { text: String::new() } };
                    self.dom.apply(&empty)?;
                }
                Ok(Some(Arc::new(patch)))
            }
            Event::Lagged { .. } => Ok(None),
        }
    }
}

/// What [`Sessions::feed`] did.
pub(crate) enum Change {
    None,
    Loaded,
    Patch(Arc<Patch>, Option<SessionEvent>),
}

/// The open sessions, the active one and the harness new ones start on.
#[derive(Default)]
pub struct Sessions {
    open: HashMap<SessionId, Session>,
    active: Option<SessionId>,
    starting: Option<SessionId>,
    /// Harness new sessions start on.
    pub harness: Option<String>,
}

impl Sessions {
    pub fn get(&self, id: &str) -> Option<&Session> {
        self.open.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.open.get_mut(id)
    }

    /// The session the chat shows.
    pub fn active(&self) -> Option<&Session> {
        self.open.get(self.active.as_ref()?)
    }

    pub fn active_mut(&mut self) -> Option<&mut Session> {
        self.open.get_mut(self.active.as_ref()?)
    }

    pub fn active_id(&self) -> Option<&SessionId> {
        self.active.as_ref()
    }

    pub fn is_active(&self, id: &str) -> bool {
        self.active.as_ref().is_some_and(|active| active.as_str() == id)
    }

    /// The session whose harness is starting or resuming.
    pub fn starting(&self) -> Option<&SessionId> {
        self.starting.as_ref()
    }

    /// Every open session, in no particular order.
    pub fn iter(&self) -> impl Iterator<Item = &Session> {
        self.open.values()
    }

    pub(crate) fn set_active(&mut self, id: Option<SessionId>) {
        self.active = id;
    }

    pub(crate) fn set_starting(&mut self, id: Option<SessionId>) {
        self.starting = id;
    }

    /// Creates a new session's journal and writer. It has no DOM root until
    /// its adapter starts.
    pub(crate) fn create(&mut self, services: &Services) -> Result<(SessionId, Arc<SessionWriter>), String> {
        let id = new_session_id();
        let live = services.library.create(&id).map_err(|error| format!("Can't create session {id}: {error}"))?;
        let writer = Arc::clone(&live.writer);
        let id = SessionId::from(id);
        self.open.insert(id.clone(), Session::new(id.clone(), live, Dom::new()));
        Ok((id, writer))
    }

    /// Opens a session from its journal unless it is open already.
    pub(crate) fn load(&mut self, services: &Services, id: &str) -> Result<(), String> {
        if self.open.contains_key(id) {
            return Ok(());
        }
        let live = services.library.open(id).map_err(|error| format!("Can't open session {id}: {error}"))?;
        let dom = live.writer.with_dom(Dom::clone);
        let id = SessionId::from(id);
        self.open.insert(id.clone(), Session::new(id, live, dom));
        Ok(())
    }

    /// Closes a session's writer and releases its journal, deleting the
    /// journal too when `delete` is set. Returns the session (its harness,
    /// if any, is for the caller to shut down) and what went wrong.
    pub(crate) fn close(&mut self, services: &Services, id: &str, delete: bool) -> (Option<Session>, Option<String>) {
        let session = self.open.remove(id);
        if self.is_active(id) {
            self.active = None;
        }
        if self.starting.as_ref().is_some_and(|starting| starting.as_str() == id) {
            self.starting = None;
        }
        let closed = if delete { services.library.delete(id) } else { services.library.close(id).map(drop) };
        (session, closed.err().map(|error| format!("Can't close session {id}: {error}")))
    }

    /// Applies what a session's feed delivered to the UI's copy.
    pub(crate) fn feed(&mut self, id: &str, feed: Feed) -> Change {
        let Some(session) = self.open.get_mut(id) else { return Change::None };
        match feed {
            Feed::Synced(dom) => {
                if dom.seq() == session.dom.seq() {
                    return Change::None;
                }
                session.dom = Arc::unwrap_or_clone(dom);
                session.busy = dom_busy(&session.dom);
                Change::Loaded
            }
            Feed::Event(event) => match session.apply(event) {
                Ok(None) => Change::None,
                Ok(Some(patch)) => {
                    if touches_turn_status(&session.dom, &patch) {
                        session.busy = dom_busy(&session.dom);
                    }
                    let event = derive(&session.id, &session.dom, &patch);
                    Change::Patch(patch, event)
                }
                Err(_) => {
                    // The copy fell out of step: take the writer's DOM.
                    session.dom = session.writer.with_dom(Dom::clone);
                    session.busy = dom_busy(&session.dom);
                    Change::Loaded
                }
            },
        }
    }

    /// One feed per open session: the writer's DOM, then every patch.
    pub(crate) fn watch(&self) -> Subscription<(SessionId, Feed)> {
        Subscription::batch(
            self.open.values().map(|session| Subscription::run_with(Watch(Arc::clone(&session.writer)), feed).with(session.id.clone())),
        )
    }
}

/// The session holds turns, or continues another session.
fn keeps_history(dom: &Dom) -> bool {
    dom_turns(dom).next().is_some() || dom.root().is_some_and(|root| root.attr(attr::PARENT_SESSION).is_some())
}

/// Lists the library's sessions for Recents, newest first, with titles from
/// their DOMs. Sessions without a turn hold nothing to keep and are deleted,
/// unless they continue another session (a fork, rewind or compaction that
/// carries its history as context).
/// Returns the summaries and a note per session that couldn't be read.
pub(crate) fn recents(library: &Library) -> (Vec<SessionSummary>, Vec<String>) {
    let mut summaries = Vec::new();
    let mut problems = Vec::new();
    let listings = match library.store().list() {
        Ok(listings) => listings,
        Err(error) => return (summaries, vec![format!("Can't list sessions: {error}")]),
    };
    for listing in listings {
        let info = match listing {
            Listing::Session(info) => info,
            Listing::Unreadable { id, error } => {
                problems.push(format!("Session {id} can't be read: {error}"));
                continue;
            }
        };
        match library.load(&info.id) {
            Ok(dom) => match SessionSummary::from_dom(&dom, false).filter(|_| keeps_history(&dom)) {
                Some(summary) => summaries.push(summary),
                None => {
                    if let Err(error) = library.delete(&info.id) {
                        problems.push(format!("Can't delete empty session {}: {error}", info.id));
                    }
                }
            },
            Err(error) => problems.push(format!("Session {} can't be read: {error}", info.id)),
        }
    }
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.updated));
    (summaries, problems)
}

/// The `<turn>` children of the session root, oldest first.
pub fn dom_turns(dom: &Dom) -> impl Iterator<Item = &loa_core::Node> {
    dom.root().into_iter().flat_map(|root| dom.children_of_kind(root.id, ElementKind::Turn))
}

/// Whether a turn runs or waits.
fn dom_busy(dom: &Dom) -> bool {
    dom_turns(dom).any(|turn| {
        matches!(turn.str_attr(attr::STATUS).and_then(TurnStatus::parse), Some(TurnStatus::Running | TurnStatus::Waiting))
    })
}

fn touches_turn_status(dom: &Dom, patch: &Patch) -> bool {
    match &patch.op {
        Op::Insert { kind, .. } => *kind == ElementKind::Turn,
        Op::Set { name, .. } => name == attr::STATUS && dom.get(patch.node).is_some_and(|node| node.kind == ElementKind::Turn),
        Op::Remove => true,
        Op::Append { .. } => false,
    }
}

/// The event `patch` (already applied to `dom`) stands for, besides itself.
fn derive(session: &SessionId, dom: &Dom, patch: &Patch) -> Option<SessionEvent> {
    let session = session.clone();
    let node = dom.get(patch.node)?;
    let status = || node.str_attr(attr::STATUS);
    match (&patch.op, node.kind) {
        (Op::Insert { .. }, ElementKind::Turn) => Some(SessionEvent::TurnStarted { session, turn: node.id }),
        (Op::Insert { .. }, ElementKind::Compaction) => Some(SessionEvent::Compaction { session, node: node.id }),
        (Op::Insert { .. } | Op::Set { .. }, ElementKind::Decision) => {
            if let Op::Set { name, .. } = &patch.op
                && name != attr::STATUS
            {
                return None;
            }
            match status().and_then(DecisionStatus::parse)? {
                DecisionStatus::Waiting => Some(SessionEvent::DecisionWaiting { session, decision: node.id }),
                DecisionStatus::Answered => Some(SessionEvent::DecisionAnswered { session, decision: node.id }),
                DecisionStatus::Expired | DecisionStatus::Cancelled => None,
            }
        }
        (Op::Set { name, .. }, ElementKind::Turn) if name == attr::STATUS => {
            match status().and_then(TurnStatus::parse)? {
                status @ (TurnStatus::Done | TurnStatus::Error | TurnStatus::Cancelled) => {
                    Some(SessionEvent::TurnEnded { session, turn: node.id, status })
                }
                TurnStatus::Running | TurnStatus::Waiting => None,
            }
        }
        _ => None,
    }
}

/// A unique session id: creation time, process and a counter.
pub(crate) fn new_session_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!("loa-{}-{}-{}", loa_core::now_ms(), std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// Identifies one writer's feed: a reopened session gets a new feed.
struct Watch(Arc<SessionWriter>);

impl Hash for Watch {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

/// The writer's DOM, then each patch; resubscribes after falling behind and
/// ends when the session closes.
fn feed(watch: &Watch) -> impl Stream<Item = Feed> + use<> {
    let writer = Arc::clone(&watch.0);
    iced::stream::channel(64, async move |mut out: iced::futures::channel::mpsc::Sender<Feed>| {
        loop {
            let (dom, mut subscription) = writer.subscribe(Policy::coalescing(FEED_CAPACITY));
            if out.send(Feed::Synced(Arc::new(dom))).await.is_err() {
                return;
            }
            loop {
                match subscription.recv().await {
                    None => return,
                    Some(Event::Lagged { .. }) => break,
                    Some(event) => {
                        if out.send(Feed::Event(event)).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    })
}
