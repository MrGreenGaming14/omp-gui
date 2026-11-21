//! `omp/session/1`: peers list, view, drive and prompt the sessions their
//! grants reach; this node stays their single writer.

use super::projection::{Access, Mirror, Projection, Update};
use crate::auth::{GrantGate, Refuse, target};
use crate::node::{Node, NodeEvent};
use crate::sessions::LocalSessions;
use crate::transport::{Connection, RecvHalf, SendHalf, Stream};
use crate::wire::{self, Ack, Envelope, Answer, Body, Drive, DriveResult, Frame, ListSessions, Prompt, Seed, SessionEntry, SessionList, Subscribe, WireUpdate};
use crate::{NodeError, NodeId, Result};
use loa_bus::{Event, NodeKey, Persist, PersistError, Policy, RequestError, SessionId, SessionWriter, WriterConfig};
use loa_core::{DecisionStatus, Dom, Draft, ElementKind, NodeId as DomNodeId, Patch, attr};
use loa_trust::{Action, PublicKey, SessionFacts};
use loa_util::LockExt;
use loa_util::time::now_ms;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// Events a subscriber may hold before it is cut off and must resubscribe.
const SUBSCRIBER_QUEUE: usize = 4096;

/// A session of a peer that this node's grants reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteSession {
    pub peer: NodeId,
    pub id: String,
    pub title: String,
    pub harness: String,
    pub created_ms: i64,
    pub actions: Vec<Action>,
    /// Only some prompts are visible.
    pub partial: bool,
    pub last_seq: u64,
}

/// This node's sessions peers may reach, and the peers' sessions last listed.
pub struct SessionShare {
    pub local: Arc<dyn LocalSessions>,
    pub remote: Mutex<HashMap<NodeId, Vec<RemoteSession>>>,
}

fn key(peer: NodeId) -> NodeKey { PublicKey::from(peer).into() }

async fn send(tx: &mut SendHalf, body: Body) -> Result<()> { wire::send(tx, &Frame::from(body)).await }

fn refused(error: impl ToString) -> Body { Body::Ack(Ack { error: error.to_string() }) }

fn ack(result: Result<()>) -> Body { refused(result.err().map(|e| e.to_string()).unwrap_or_default()) }

pub(crate) async fn serve(node: Node, conn: Connection, mut stream: Stream) -> Result<()> {
    let peer = conn.remote();
    let request = wire::expect::<Frame>(&mut stream.recv).await?.body;
    node.0.grants.challenge(peer, &mut stream).await?;
    match request {
        Some(Body::ListSessions(_)) => send(&mut stream.send, Body::SessionList(SessionList { sessions: visible(&node, peer) })).await,
        Some(Body::Prompt(prompt)) => send(&mut stream.send, ack(prompted(&node, peer, prompt))).await,
        Some(Body::Subscribe(subscribe)) => subscription(&node, peer, subscribe, stream).await,
        _ => send(&mut stream.send, refused("not a request")).await,
    }
}

fn visible(node: &Node, peer: NodeId) -> Vec<SessionEntry> {
    let (access, peer) = (&node.0.grants.access, key(peer));
    let sessions = node.0.share.local.sessions().into_iter().filter_map(|session| {
        let actions = access.session_actions(&peer, &session.facts);
        let partial = actions.is_empty() && session.turns.iter().any(|t| access.turn_visible(&peer, &session.facts, t.id));
        (!actions.is_empty() || partial).then(|| SessionEntry {
            title: if actions.contains(&Action::View) { session.label() } else { String::new() },
            id: session.facts.id,
            harness: session.harness,
            created_ms: session.facts.created_ms,
            actions: actions.iter().map(|a| a.as_str().to_owned()).collect(),
            partial,
            last_seq: session.last_seq,
        })
    });
    sessions.collect()
}

fn prompted(node: &Node, peer: NodeId, prompt: Prompt) -> Result<()> {
    let writer = node.0.share.local.writer(&prompt.session).ok_or_else(|| NodeError::Refused("no such session".into()))?;
    if !writer.with_dom(|dom| node.0.grants.access.allows_in(&key(peer), dom, Action::Drive, None)) {
        return Err(NodeError::Refused("the grant does not allow driving this session".into()));
    }
    if prompt.text.trim().is_empty() {
        return Err(NodeError::Refused("empty prompt".into()));
    }
    node.emit(NodeEvent::PromptRequested { peer, session: prompt.session, text: prompt.text });
    Ok(())
}

/// Checks a peer's answer to a waiting decision and hands it to the app.
fn answered(node: &Node, peer: NodeId, session: &str, projection: &Projection, writer: &SessionWriter, answer: Answer) -> Result<()> {
    let refuse = |text: &str| Err(NodeError::Refused(text.into()));
    if answer.session != session {
        return refuse("an answer on this stream must name its session");
    }
    let Some(decision) = projection.source_node(DomNodeId(answer.decision)) else { return refuse("the decision is outside the grant") };
    let (allowed, waiting) = writer.with_dom(|dom| {
        let waiting = dom.get(decision).is_some_and(|n| n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str()));
        (node.0.grants.access.allows_in(&key(peer), dom, Action::Drive, Some(decision)), waiting)
    });
    match (allowed, waiting) {
        (false, _) => refuse("the grant does not allow answering this decision"),
        (_, false) => refuse("the decision is no longer waiting"),
        _ => {
            node.emit(NodeEvent::AnswerRequested { peer, session: session.to_owned(), decision, values: answer.values, reason: answer.reason });
            Ok(())
        }
    }
}

/// Streams the requester's projection of a session and applies its requests
/// in order, until either side ends or its grants do.
async fn subscription(node: &Node, peer: NodeId, subscribe: Subscribe, stream: Stream) -> Result<()> {
    let Stream { send: mut tx, recv: mut rx } = stream;
    let access = Arc::clone(&node.0.grants.access);
    let (writer, key) = (node.0.share.local.writer(&subscribe.session), key(peer));
    let readable = |dom: &Dom| access.allows_in(&key, dom, Action::View, None) || dom.iter().any(|n| n.kind == ElementKind::Turn && access.allows_in(&key, dom, Action::View, Some(n.id)));
    let Some(writer) = writer.filter(|w| w.is_writer()) else { return send(&mut tx, refused("no such session")).await };
    if !writer.with_dom(readable) {
        return send(&mut tx, refused(format!("no view grant on {}", subscribe.session))).await;
    }
    let (dom, mut events) = writer.subscribe(Policy::every_patch(SUBSCRIBER_QUEUE));
    let actions = SessionFacts::from_dom(&dom).map(|facts| access.session_actions(&key, &facts)).unwrap_or_default();
    let (mut projection, seed) = Projection::seeded(key, &dom, &*access)?;
    send(&mut tx, Body::Seed(Seed::new(dom.seq(), &seed, &actions))).await?;
    drop(dom);
    // Frames are read on their own task: reading is not cancel safe.
    let (requests_tx, mut requests) = tokio::sync::mpsc::channel(16);
    let reader = tokio::spawn(async move {
        while let Ok(Some(Frame { body: Some(body) })) = wire::recv(&mut rx).await {
            if requests_tx.send(body).await.is_err() {
                break;
            }
        }
    });
    let ended = loop {
        let answer = tokio::select! {
            event = events.recv() => match event {
                Some(Event::Patch(patch)) if !access.live_grants(&key, now_ms()).is_empty() => {
                    let update = projection.accept(&patch, &*access)?;
                    Body::Update(WireUpdate { source_seq: update.source_seq, patch: update.patch.as_ref().map(Into::into) })
                }
                Some(Event::Patch(_)) => break Some("the grant ended"),
                Some(Event::Lagged { .. }) => break Some("fell behind; open the session again"),
                Some(Event::Appends(_)) => unreachable!("every_patch subscribers get single patches"),
                None => break Some("the session closed"),
            },
            request = requests.recv() => match request {
                Some(Body::Drive(drive)) => Body::DriveResult(driven(&writer, &projection, &*access, key, drive)),
                Some(Body::Prompt(prompt)) if prompt.session != subscribe.session => refused("a prompt on this stream must name its session"),
                Some(Body::Prompt(prompt)) => ack(prompted(node, peer, prompt)),
                Some(Body::Answer(answer)) => ack(answered(node, peer, &subscribe.session, &projection, &writer, answer)),
                Some(_) => refused("not a request on a subscription"),
                None => break None,
            },
        };
        send(&mut tx, answer).await?;
    };
    reader.abort();
    if let Some(reason) = ended {
        let _ = send(&mut tx, refused(reason)).await;
    }
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut tx).await;
    Ok(())
}

fn driven(writer: &SessionWriter, projection: &Projection, access: &dyn Access, peer: NodeKey, drive: Drive) -> DriveResult {
    let failed = |error: String| DriveResult { patch: None, conflict_current: None, error };
    let draft = match serde_json::from_slice::<Draft>(&drive.draft) {
        Ok(draft) => draft,
        Err(error) => return failed(error.to_string()),
    };
    let Some(draft) = projection.source_draft(draft) else { return failed("the change names a node outside the grant".into()) };
    if !writer.with_dom(|dom| access.allows(peer, dom, Action::Drive, Some(target(&draft)))) {
        return failed(format!("no drive grant on {}", writer.session()));
    }
    match writer.request(&peer, Some(drive.base_seq), draft, now_ms()) {
        Ok(patch) => DriveResult { patch: Some((&patch).into()), conflict_current: None, error: String::new() },
        Err(RequestError::Conflict { actual, .. }) => DriveResult { patch: None, conflict_current: Some(actual), error: "the session moved on".into() },
        Err(error) => failed(error.to_string()),
    }
}

/// The requester side: a stream with `request` sent and grants presented.
async fn request(node: &Node, peer: NodeId, request: Body) -> Result<(Connection, Stream)> {
    let conn = node.dial(peer, wire::SESSION).await?;
    let mut stream = conn.open_stream().await?;
    send(&mut stream.send, request).await?;
    node.0.grants.present(peer, &mut stream).await?;
    Ok((conn, stream))
}

/// One request and its answer on a fresh stream.
async fn once(node: &Node, peer: NodeId, body: Body) -> Result<Option<Body>> {
    let (conn, mut stream) = request(node, peer, body).await?;
    let answer = wire::expect::<Frame>(&mut stream.recv).await;
    conn.close(0, b"done");
    Ok(answer?.body)
}

pub(crate) async fn list(node: &Node, peer: NodeId) -> Result<Vec<RemoteSession>> {
    let Some(Body::SessionList(list)) = once(node, peer, Body::ListSessions(ListSessions {})).await? else {
        return Err(NodeError::Protocol("expected the session list".into()));
    };
    Ok(list.sessions.into_iter().map(|entry| RemoteSession {
        peer,
        actions: wire::actions(&entry.actions),
        id: entry.id,
        title: entry.title,
        harness: entry.harness,
        created_ms: entry.created_ms,
        partial: entry.partial,
        last_seq: entry.last_seq,
    }).collect())
}

pub(crate) async fn prompt(node: &Node, peer: NodeId, session: &str, text: &str) -> Result<()> {
    once(node, peer, Body::Prompt(Prompt { session: session.to_owned(), text: text.to_owned() })).await.map(drop)
}

/// Keeps the mirror's own patches, so a fork can copy them.
struct Recorder(Arc<Mutex<Vec<Patch>>>);

impl Persist for Recorder {
    fn append(&mut self, patch: &Patch) -> Result<(), PersistError> {
        self.0.locked().push(patch.clone());
        Ok(())
    }
}

/// What came of a drive request.
#[derive(Clone, Debug, PartialEq)]
pub enum DriveOutcome {
    /// The owner applied it as its patch `source_seq`; it reaches the view as an update.
    Applied { source_seq: u64 },
    /// The owner's session moved past the request's base; retry on the new state.
    Conflict { current: u64 },
}

type Pending = Arc<Mutex<VecDeque<oneshot::Sender<Result<Body>>>>>;

/// A live, read-only mirror of a peer's session. Changes go to the owner as
/// drive requests; this node never writes the session itself.
pub struct RemoteView {
    peer: NodeId,
    session: String,
    actions: Vec<Action>,
    writer: Arc<SessionWriter>,
    patches: Arc<Mutex<Vec<Patch>>>,
    source_seq: Arc<AtomicU64>,
    send: tokio::sync::Mutex<SendHalf>,
    pending: Pending,
    ended: Arc<Mutex<Option<String>>>,
    pump: tokio::task::JoinHandle<()>,
    conn: Connection,
}

impl Drop for RemoteView {
    fn drop(&mut self) {
        self.pump.abort();
        self.conn.close(0, b"closed");
    }
}

impl std::fmt::Debug for RemoteView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteView").field("peer", &self.peer).field("session", &self.session).field("actions", &self.actions).finish_non_exhaustive()
    }
}

pub(crate) async fn open(node: &Node, peer: NodeId, session: &str) -> Result<RemoteView> {
    let (conn, stream) = request(node, peer, Body::Subscribe(Subscribe { session: session.to_owned(), after_source_seq: 0 })).await?;
    let Stream { send: tx, recv: mut rx } = stream;
    let Some(Body::Seed(seed)) = wire::expect::<Frame>(&mut rx).await?.body else { return Err(NodeError::Protocol("expected the session seed".into())) };
    let patches = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::new(SessionWriter::new(WriterConfig {
        session: SessionId::from(session),
        owner: key(peer),
        local: key(node.key()),
        dom: Dom::new(),
        persist: Box::new(Recorder(Arc::clone(&patches))),
        authorize: Arc::new(Refuse),
    }));
    let mirror = Mirror::seeded(Arc::clone(&writer), seed.source_seq, wire::patches(seed.patches)?)?;
    let (source_seq, pending, ended) = (Arc::new(AtomicU64::new(seed.source_seq)), Pending::default(), Arc::new(Mutex::new(None)));
    let pump = tokio::spawn(pump(rx, mirror, Arc::clone(&source_seq), Arc::clone(&pending), Arc::clone(&ended)));
    let actions = wire::actions(&seed.actions);
    Ok(RemoteView { peer, session: session.to_owned(), actions, writer, patches, source_seq, send: tokio::sync::Mutex::new(tx), pending, ended, pump, conn })
}

async fn pump(mut rx: RecvHalf, mut mirror: Mirror, source: Arc<AtomicU64>, pending: Pending, ended: Arc<Mutex<Option<String>>>) {
    let reason = loop {
        let body = match wire::recv::<Frame>(&mut rx).await {
            Ok(Some(Frame { body: Some(body) })) => body,
            Ok(_) => break "the owner closed the session".to_owned(),
            Err(error) => break error.to_string(),
        };
        if let Body::Update(update) = body {
            let applied = wire::patches(update.patch.into_iter().collect()).and_then(|patch| mirror.receive(Update { source_seq: update.source_seq, patch: patch.into_iter().next() }));
            if let Err(error) = applied {
                break error.to_string();
            }
            source.store(mirror.source_seq(), Ordering::Release);
            continue;
        }
        let answer = Frame::from(body).into_ack().map(|frame| frame.body.expect("an ack has a body"));
        match (pending.locked().pop_front(), answer) {
            (Some(waiter), answer) => drop(waiter.send(answer)),
            (None, Err(error)) => break error.to_string(),
            (None, Ok(_)) => break "unexpected answer".to_owned(),
        }
    };
    *ended.locked() = Some(reason);
    pending.locked().clear();
    mirror.writer().close();
}

impl RemoteView {
    pub fn peer(&self) -> NodeId { self.peer }
    pub fn session(&self) -> &str { &self.session }
    /// The mirror: subscribe to it like any session. It takes no local commits.
    pub fn writer(&self) -> &Arc<SessionWriter> { &self.writer }
    /// What the grants allow on the whole session.
    pub fn actions(&self) -> &[Action] { &self.actions }
    pub fn can(&self, action: Action) -> bool { self.actions.contains(&action) }
    /// The owner's sequence this view has caught up with; the base of drive requests.
    pub fn source_seq(&self) -> u64 { self.source_seq.load(Ordering::Acquire) }
    /// Why the view stopped following the owner, once it has.
    pub fn ended(&self) -> Option<String> { self.ended.locked().clone() }
    /// The mirror's patches from 1, for forking.
    pub(crate) fn patches(&self) -> Vec<Patch> { self.patches.locked().clone() }

    async fn call(&self, body: Body) -> Result<Body> {
        let (tx, rx) = oneshot::channel();
        {
            let mut send_half = self.send.lock().await;
            if let Some(reason) = self.ended() {
                return Err(NodeError::Refused(reason));
            }
            self.pending.locked().push_back(tx);
            send(&mut send_half, body).await?;
        }
        rx.await.map_err(|_| NodeError::Refused(self.ended().unwrap_or_else(|| "the view closed".into())))?
    }

    /// Asks the owner to apply `draft`, written against this view, on the owner state it has seen.
    pub async fn drive(&self, draft: Draft) -> Result<DriveOutcome> { self.drive_at(self.source_seq(), draft).await }

    /// [`RemoteView::drive`] on an explicit owner sequence.
    pub async fn drive_at(&self, base_seq: u64, draft: Draft) -> Result<DriveOutcome> {
        let draft = serde_json::to_vec(&draft).map_err(|e| NodeError::Protocol(e.to_string()))?;
        match self.call(Body::Drive(Drive { session: self.session.clone(), base_seq, draft, ts: now_ms() })).await? {
            Body::DriveResult(DriveResult { conflict_current: Some(current), .. }) => Ok(DriveOutcome::Conflict { current }),
            Body::DriveResult(DriveResult { patch: Some(patch), .. }) => Ok(DriveOutcome::Applied { source_seq: patch.seq }),
            Body::DriveResult(result) => Err(NodeError::Refused(result.error)),
            _ => Err(NodeError::Protocol("expected a drive result".into())),
        }
    }

    /// Asks the owner's harness to run `text` in this session.
    pub async fn prompt(&self, text: &str) -> Result<()> {
        self.call(Body::Prompt(Prompt { session: self.session.clone(), text: text.to_owned() })).await.map(drop)
    }

    /// Answers a waiting decision (its id in this view) on the owner, as this node (needs drive).
    pub async fn answer(&self, decision: DomNodeId, values: Vec<String>, reason: Option<String>) -> Result<()> {
        self.call(Body::Answer(Answer { session: self.session.clone(), decision: decision.0, values, reason })).await.map(drop)
    }
}
