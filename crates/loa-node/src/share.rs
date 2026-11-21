//! `omp/session/1` for the node: peers list, view, drive and prompt the
//! sessions their grants reach; this node stays their single writer.

use loa_util::LockExt;
use crate::access::Refuse;
use crate::{Inner, Node, NodeError, NodeEvent, RemoteSession, Result, bus_key, grant_key, now_ms};
use async_trait::async_trait;
use loa_bus::{Event, Persist, PersistError, Policy, SessionId, SessionWriter, WriterConfig};
use loa_core::{DecisionStatus, Dom, Draft, ElementKind, NodeId as DomNodeId, Patch, attr};
use loa_grants::{Action, Challenge, Presented, Proof, SessionFacts, Verifier, prove};
use loa_net::NodeId;
use loa_net::alpn;
use loa_net::conn::{Connection, RecvHalf, SendHalf, Stream};
use loa_net::control::{recv_message, send_message};
use loa_net::router::ProtocolHandler;
use loa_share::{
    Ack, Drive, DriveResult, Frame, ListSessions, Mirror, Owner, Present, PresentedGrant, Projection, Prompt, Seed,
    SessionEntry, SessionList, Subscribe, Update, WireUpdate, frame::Body,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::oneshot;

/// Events a subscriber may hold before it is cut off and must resubscribe.
const SUBSCRIBER_QUEUE: usize = 4096;


pub(crate) async fn send(send: &mut SendHalf, body: Body) -> Result<()> {
    Ok(send_message(send, &Frame::from(body)).await?)
}

async fn recv(recv: &mut RecvHalf) -> Result<Option<Body>> {
    Ok(recv_message::<Frame>(recv).await?.and_then(|frame| frame.body))
}

pub(crate) async fn expect(recv_half: &mut RecvHalf) -> Result<Body> {
    recv(recv_half).await?.ok_or_else(|| NodeError::Protocol("the stream ended early".into()))
}

fn refused(error: impl ToString) -> Body {
    Body::Ack(Ack { error: error.to_string() })
}

/// Serves session streams on accepted connections.
pub(crate) struct Handler(pub(crate) Weak<Inner>);

#[async_trait]
impl ProtocolHandler for Handler {
    async fn handle(&self, conn: Connection) {
        while let Ok(stream) = conn.accept_stream().await {
            let Some(inner) = self.0.upgrade() else { return };
            let node = inner.node();
            let conn = conn.clone();
            tokio::spawn(async move {
                if let Err(error) = serve(&node, conn.remote(), stream).await {
                    tracing::debug!(peer = %conn.remote(), %error, "session stream ended");
                }
            });
        }
    }
}

async fn serve(node: &Node, peer: NodeId, mut stream: Stream) -> Result<()> {
    let request = expect(&mut stream.recv).await?;
    authenticate(node, peer, &mut stream).await?;
    match request {
        Body::ListSessions(_) => {
            let sessions = visible_sessions(node, peer);
            send(&mut stream.send, Body::SessionList(SessionList { sessions })).await
        }
        Body::Prompt(prompt) => {
            let answer = requested_prompt(node, peer, prompt).err().map(|e| e.to_string()).unwrap_or_default();
            send(&mut stream.send, Body::Ack(Ack { error: answer })).await
        }
        Body::Subscribe(subscribe) => subscription(node, peer, subscribe, stream).await,
        _ => send(&mut stream.send, refused("not a request")).await,
    }
}

/// Challenges the requester and records the grants it proves it holds.
async fn authenticate(node: &Node, peer: NodeId, stream: &mut Stream) -> Result<()> {
    let verified = challenge_grants(node, peer, stream).await?;
    node.access().set(bus_key(&peer), verified);
    Ok(())
}

/// Challenges the requester on `stream` and returns the grants it proves it
/// holds from this node.
pub(crate) async fn challenge_grants(node: &Node, peer: NodeId, stream: &mut Stream) -> Result<Vec<loa_grants::VerifiedGrant>> {
    let challenge = Challenge::random()?;
    send(&mut stream.send, Body::Challenge(loa_share::Challenge { nonce: challenge.0.to_vec() })).await?;
    let Body::Present(present) = expect(&mut stream.recv).await? else {
        return Err(NodeError::Protocol("expected the requester's grants".into()));
    };
    let requester = grant_key(&peer);
    let verifier = Verifier::new(grant_key(&node.key()), Arc::clone(node.grant_store()));
    let now = now_ms();
    Ok(present
        .grants
        .iter()
        .filter_map(|grant| {
            let proof = Proof(grant.proof.as_slice().try_into().ok()?);
            verifier
                .verify(Presented { token: &grant.token, proof: &proof }, &requester, &challenge, now)
                .inspect_err(|denied| tracing::debug!(%peer, %denied, "presented grant refused"))
                .ok()
        })
        .collect())
}

fn visible_sessions(node: &Node, peer: NodeId) -> Vec<SessionEntry> {
    let access = node.access();
    let key = bus_key(&peer);
    node.local_sessions()
        .sessions()
        .into_iter()
        .filter_map(|session| {
            let actions = access.session_actions(&key, &session.facts);
            let partial = actions.is_empty() && session.turns.iter().any(|t| access.turn_visible(&key, &session.facts, t.id));
            (!actions.is_empty() || partial).then(|| SessionEntry {
                title: if actions.contains(&Action::View) { session.label() } else { String::new() },
                id: session.facts.id,
                harness: session.harness,
                created_ms: session.facts.created_ms,
                actions: actions.iter().map(|a| a.as_str().to_owned()).collect(),
                partial,
                last_seq: session.last_seq,
            })
        })
        .collect()
}

fn requested_prompt(node: &Node, peer: NodeId, prompt: Prompt) -> Result<()> {
    let writer = node.local_sessions().writer(&prompt.session).ok_or_else(|| NodeError::Refused("no such session".into()))?;
    let allowed = writer.with_dom(|dom| loa_share::Access::allows(&**node.access(), bus_key(&peer), dom, Action::Drive, None));
    if !allowed {
        return Err(NodeError::Refused("the grant does not allow driving this session".into()));
    }
    if prompt.text.trim().is_empty() {
        return Err(NodeError::Refused("empty prompt".into()));
    }
    node.emit(NodeEvent::PromptRequested { peer, session: prompt.session, text: prompt.text });
    Ok(())
}

/// Checks a peer's answer to a waiting decision of `session` and hands it to
/// the app, which answers through the harness waiting on it.
fn requested_answer(node: &Node, peer: NodeId, session: &str, projection: &Projection, writer: &SessionWriter, answer: loa_share::Answer) -> Result<()> {
    if answer.session != session {
        return Err(NodeError::Refused("an answer on this stream must name its session".into()));
    }
    let decision = projection
        .source_node(DomNodeId(answer.decision))
        .ok_or_else(|| NodeError::Refused("the decision is outside the grant".into()))?;
    let (allowed, waiting) = writer.with_dom(|dom| {
        let allowed = loa_share::Access::allows(&**node.access(), bus_key(&peer), dom, Action::Drive, Some(decision));
        let waiting = dom.get(decision).is_some_and(|n| {
            n.kind == ElementKind::Decision && n.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str())
        });
        (allowed, waiting)
    });
    if !allowed {
        return Err(NodeError::Refused("the grant does not allow answering this decision".into()));
    }
    if !waiting {
        return Err(NodeError::Refused("the decision is no longer waiting".into()));
    }
    node.emit(NodeEvent::AnswerRequested {
        peer,
        session: session.to_owned(),
        decision,
        values: answer.values,
        reason: answer.reason,
    });
    Ok(())
}

/// Streams the requester's projection of a session and applies its drive
/// requests in order, until either side ends or its grants do.
async fn subscription(node: &Node, peer: NodeId, subscribe: Subscribe, stream: Stream) -> Result<()> {
    let Stream { send: mut tx, recv: mut rx } = stream;
    let Some(writer) = node.local_sessions().writer(&subscribe.session) else {
        return send(&mut tx, refused("no such session")).await;
    };
    let access = Arc::clone(node.access());
    let key = bus_key(&peer);
    let owner = Owner { writer: &writer, access: &*access };
    if let Err(error) = owner.stream(key) {
        return send(&mut tx, refused(error)).await;
    }
    let (dom, mut events) = writer.subscribe(Policy::every_patch(SUBSCRIBER_QUEUE));
    let actions = SessionFacts::from_dom(&dom).map(|facts| access.session_actions(&key, &facts)).unwrap_or_default();
    let (mut projection, seed) = Projection::seeded(key, writer.session().clone(), &dom, &*access)?;
    send(&mut tx, Body::Seed(Seed::new(dom.seq(), &seed, &actions))).await?;
    drop(dom);

    // Frames are read on their own task: reading is not cancel safe.
    let (requests_tx, mut requests) = tokio::sync::mpsc::channel(16);
    let reader = tokio::spawn(async move {
        while let Ok(Some(body)) = recv(&mut rx).await {
            if requests_tx.send(body).await.is_err() {
                break;
            }
        }
    });
    let ended = loop {
        tokio::select! {
            event = events.recv() => match event {
                Some(Event::Patch(patch)) => {
                    if !access.has_live(&key) {
                        break Some("the grant ended".to_owned());
                    }
                    let update = projection.accept(&patch, &*access)?;
                    send(&mut tx, Body::Update(WireUpdate::from(&update))).await?;
                }
                Some(Event::Lagged { .. }) => break Some("fell behind; open the session again".to_owned()),
                Some(Event::Appends(_)) => unreachable!("every_patch subscribers get single patches"),
                None => break Some("the session closed".to_owned()),
            },
            request = requests.recv() => match request {
                Some(Body::Drive(drive)) => {
                    let result = drive_request(&owner, &projection, key, drive);
                    send(&mut tx, Body::DriveResult(result)).await?;
                }
                Some(Body::Prompt(prompt)) => {
                    let error = if prompt.session == subscribe.session {
                        requested_prompt(node, peer, prompt).err().map(|e| e.to_string()).unwrap_or_default()
                    } else {
                        "a prompt on this stream must name its session".to_owned()
                    };
                    send(&mut tx, Body::Ack(Ack { error })).await?;
                }
                Some(Body::Answer(answer)) => {
                    let error = requested_answer(node, peer, &subscribe.session, &projection, &writer, answer)
                        .err()
                        .map(|e| e.to_string())
                        .unwrap_or_default();
                    send(&mut tx, Body::Ack(Ack { error })).await?;
                }
                Some(_) => send(&mut tx, refused("not a request on a subscription")).await?,
                None => break None,
            },
        }
    };
    reader.abort();
    if let Some(reason) = ended {
        let _ = send(&mut tx, refused(reason)).await;
    }
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut tx).await;
    Ok(())
}

fn drive_request(owner: &Owner<'_>, projection: &Projection, peer: loa_bus::NodeKey, drive: Drive) -> DriveResult {
    let failed = |error: String| DriveResult { patch: None, conflict_current: None, error };
    let draft = match drive.draft() {
        Ok(draft) => draft,
        Err(error) => return failed(error.to_string()),
    };
    let Some(draft) = projection.source_draft(draft) else {
        return failed("the change names a node outside the grant".into());
    };
    match owner.request(peer, drive.base_seq, draft, now_ms()) {
        Ok(patch) => DriveResult { patch: Some((&patch).into()), conflict_current: None, error: String::new() },
        Err(loa_share::Error::Conflict { current, .. }) => {
            DriveResult { patch: None, conflict_current: Some(current), error: "the session moved on".into() }
        }
        Err(error) => failed(error.to_string()),
    }
}

/// The requester side: a stream with `request` sent and grants presented.
async fn request(node: &Node, peer: NodeId, request: Body) -> Result<(Connection, Stream)> {
    let conn = node.dial(peer, alpn::SESSION).await?;
    let mut stream = conn.open_stream().await?;
    send(&mut stream.send, request).await?;
    present_grants(node, peer, &mut stream).await?;
    Ok((conn, stream))
}

/// Answers the owner's challenge on `stream` with every live grant this node
/// holds from `peer`.
pub(crate) async fn present_grants(node: &Node, peer: NodeId, stream: &mut Stream) -> Result<()> {
    let Body::Challenge(challenge) = expect(&mut stream.recv).await? else {
        return Err(NodeError::Protocol("expected a challenge".into()));
    };
    let challenge = Challenge(challenge.nonce.as_slice().try_into().map_err(|_| NodeError::Protocol("challenge is not 32 bytes".into()))?);
    let root = grant_key(&peer);
    let now = now_ms();
    let grants = node
        .grant_store()
        .received_all()?
        .into_iter()
        .filter(|g| g.root == root && g.refused_ms.is_none() && g.expires_ms > now)
        .map(|g| PresentedGrant { proof: prove(node.grant_signer(), &challenge, &g.token).0.to_vec(), token: g.token })
        .collect();
    send(&mut stream.send, Body::Present(Present { grants })).await
}

pub(crate) async fn list(node: &Node, peer: NodeId) -> Result<Vec<RemoteSession>> {
    let (conn, mut stream) = request(node, peer, Body::ListSessions(ListSessions {})).await?;
    let answer = expect(&mut stream.recv).await;
    conn.close(0, b"done");
    match answer? {
        Body::SessionList(list) => Ok(list
            .sessions
            .into_iter()
            .map(|entry| RemoteSession {
                peer,
                actions: entry.actions.iter().filter_map(|a| Action::parse(a)).collect(),
                id: entry.id,
                title: entry.title,
                harness: entry.harness,
                created_ms: entry.created_ms,
                partial: entry.partial,
                last_seq: entry.last_seq,
            })
            .collect()),
        Body::Ack(ack) => Err(NodeError::Refused(ack.error)),
        _ => Err(NodeError::Protocol("expected the session list".into())),
    }
}

pub(crate) async fn prompt(node: &Node, peer: NodeId, session: &str, text: &str) -> Result<()> {
    let body = Body::Prompt(Prompt { session: session.to_owned(), text: text.to_owned() });
    let (conn, mut stream) = request(node, peer, body).await?;
    let answer = expect(&mut stream.recv).await;
    conn.close(0, b"done");
    match answer? {
        Body::Ack(ack) if ack.error.is_empty() => Ok(()),
        Body::Ack(ack) => Err(NodeError::Refused(ack.error)),
        _ => Err(NodeError::Protocol("expected an acknowledgement".into())),
    }
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
    /// The owner applied it as its patch `source_seq`; the change reaches the
    /// view as an update.
    Applied { source_seq: u64 },
    /// The owner's session moved past the request's base; retry on the new state.
    Conflict { current: u64 },
}

type Pending = Arc<Mutex<VecDeque<oneshot::Sender<Body>>>>;

/// A live, read-only mirror of a peer's session, kept current by the owner's
/// updates. Changes go to the owner as drive requests; this node never writes
/// the session itself.
pub struct RemoteView {
    peer: NodeId,
    session: String,
    actions: Vec<Action>,
    writer: Arc<SessionWriter>,
    patches: Arc<Mutex<Vec<Patch>>>,
    source_seq: Arc<AtomicU64>,
    send: Arc<tokio::sync::Mutex<SendHalf>>,
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
    let subscribe = Body::Subscribe(Subscribe { session: session.to_owned(), after_source_seq: 0 });
    let (conn, stream) = request(node, peer, subscribe).await?;
    let Stream { send: tx, recv: mut rx } = stream;
    let seed = match expect(&mut rx).await? {
        Body::Seed(seed) => seed,
        Body::Ack(ack) => return Err(NodeError::Refused(ack.error)),
        _ => return Err(NodeError::Protocol("expected the session seed".into())),
    };
    let actions: Vec<Action> = seed.actions.iter().filter_map(|a| Action::parse(a)).collect();
    let source_seq = seed.source_seq;
    let patches = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::new(SessionWriter::new(WriterConfig {
        session: SessionId::from(session),
        owner: bus_key(&peer),
        local: bus_key(&node.key()),
        dom: Dom::new(),
        persist: Box::new(Recorder(Arc::clone(&patches))),
        authorize: Arc::new(Refuse),
    }));
    let mirror = Mirror::seeded(Arc::clone(&writer), source_seq, seed.patches()?)?;
    let source = Arc::new(AtomicU64::new(source_seq));
    let pending: Pending = Arc::default();
    let ended = Arc::new(Mutex::new(None));
    let pump = tokio::spawn(pump(rx, mirror, Arc::clone(&source), Arc::clone(&pending), Arc::clone(&ended)));
    Ok(RemoteView {
        peer,
        session: session.to_owned(),
        actions,
        writer,
        patches,
        source_seq: source,
        send: Arc::new(tokio::sync::Mutex::new(tx)),
        pending,
        ended,
        pump,
        conn,
    })
}

async fn pump(mut rx: RecvHalf, mut mirror: Mirror, source: Arc<AtomicU64>, pending: Pending, ended: Arc<Mutex<Option<String>>>) {
    let reason = loop {
        match recv(&mut rx).await {
            Ok(Some(Body::Update(update))) => {
                let applied = Update::try_from(update).and_then(|update| mirror.receive(update));
                if let Err(error) = applied {
                    break error.to_string();
                }
                source.store(mirror.source_seq(), Ordering::Release);
            }
            Ok(Some(answer @ (Body::DriveResult(_) | Body::Ack(_)))) => {
                let waiter = pending.locked().pop_front();
                match (waiter, answer) {
                    (Some(waiter), answer) => {
                        let _ = waiter.send(answer);
                    }
                    (None, Body::Ack(ack)) => break ack.error,
                    (None, _) => break "unexpected answer".to_owned(),
                }
            }
            Ok(Some(_)) => break "unexpected frame".to_owned(),
            Ok(None) => break "the owner closed the session".to_owned(),
            Err(error) => break error.to_string(),
        }
    };
    *ended.locked() = Some(reason);
    pending.locked().clear();
    mirror.writer().close();
}

impl RemoteView {
    pub fn peer(&self) -> NodeId {
        self.peer
    }

    pub fn session(&self) -> &str {
        &self.session
    }

    /// The mirror: subscribe to it like any session. It takes no local commits.
    pub fn writer(&self) -> &Arc<SessionWriter> {
        &self.writer
    }

    /// What the grants allow on the whole session.
    pub fn actions(&self) -> &[Action] {
        &self.actions
    }

    pub fn can(&self, action: Action) -> bool {
        self.actions.contains(&action)
    }

    /// The owner's sequence this view has caught up with; the base of drive requests.
    pub fn source_seq(&self) -> u64 {
        self.source_seq.load(Ordering::Acquire)
    }

    /// Why the view stopped following the owner, once it has.
    pub fn ended(&self) -> Option<String> {
        self.ended.locked().clone()
    }

    /// The mirror's patches from 1, for forking.
    pub(crate) fn patches(&self) -> Vec<Patch> {
        self.patches.locked().clone()
    }

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
        rx.await.map_err(|_| NodeError::Refused(self.ended().unwrap_or_else(|| "the view closed".into())))
    }

    /// Asks the owner to apply `draft`, written against this view's DOM, on
    /// top of the owner state this view has seen.
    pub async fn drive(&self, draft: Draft) -> Result<DriveOutcome> {
        self.drive_at(self.source_seq(), draft).await
    }

    /// [`RemoteView::drive`] on an explicit owner sequence.
    pub async fn drive_at(&self, base_seq: u64, draft: Draft) -> Result<DriveOutcome> {
        let request = Drive::new(&self.session, base_seq, &draft, now_ms())?;
        match self.call(Body::Drive(request)).await? {
            Body::DriveResult(DriveResult { conflict_current: Some(current), .. }) => Ok(DriveOutcome::Conflict { current }),
            Body::DriveResult(DriveResult { patch: Some(patch), .. }) => Ok(DriveOutcome::Applied { source_seq: patch.seq }),
            Body::DriveResult(result) => Err(NodeError::Refused(result.error)),
            Body::Ack(ack) => Err(NodeError::Refused(ack.error)),
            _ => Err(NodeError::Protocol("expected a drive result".into())),
        }
    }

    /// Asks the owner's harness to run `text` in this session.
    pub async fn prompt(&self, text: &str) -> Result<()> {
        match self.call(Body::Prompt(Prompt { session: self.session.clone(), text: text.to_owned() })).await? {
            Body::Ack(ack) if ack.error.is_empty() => Ok(()),
            Body::Ack(ack) => Err(NodeError::Refused(ack.error)),
            _ => Err(NodeError::Protocol("expected an acknowledgement".into())),
        }
    }

    /// Answers a waiting decision of this session (its id in this view) on
    /// the owner: the owner's harness takes it as if answered there, with
    /// this node as `by` (needs drive).
    pub async fn answer(&self, decision: DomNodeId, values: Vec<String>, reason: Option<String>) -> Result<()> {
        let body = Body::Answer(loa_share::Answer { session: self.session.clone(), decision: decision.0, values, reason });
        match self.call(body).await? {
            Body::Ack(ack) if ack.error.is_empty() => Ok(()),
            Body::Ack(ack) => Err(NodeError::Refused(ack.error)),
            _ => Err(NodeError::Protocol("expected an acknowledgement".into())),
        }
    }
}
