//! Owner-ordered patch sharing. The app binds the ports here to the authenticated
//! `omp/session/1` connection, journal reader, and verified Biscuit grants.
//!
//! A receiver never assigns a sequence number to a remote session. Requests go
//! to the owner; only committed patches come back. A partial grant receives an
//! independently numbered projection, never the owner's hidden patches.

use loa_bus::{NodeKey, SessionId, SessionWriter};
use loa_core::{attr, Dom, Draft, ElementKind, NodeId, Op, Patch, Value};
use loa_grants::Action;
use loa_journal::{Journal, Store};
use prost::Message;
use std::collections::HashMap;
use std::sync::Arc;

pub const SESSION_ALPN: &[u8] = b"omp/session/1";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("grant refused {0:?} on session {1}")]
    Denied(Action, SessionId),
    #[error("this node does not own the session")]
    NotOwner,
    #[error("patch {got} is not next after {last}")]
    Sequence { last: u64, got: u64 },
    #[error("request conflicts with owner's patch sequence: expected {expected}, current {current}")]
    Conflict { expected: u64, current: u64 },
    #[error("session has no root")]
    NoSession,
    #[error("wire message is invalid: {0}")]
    Wire(String),
    #[error("journal: {0}")]
    Journal(#[from] loa_journal::Error),
    #[error("DOM: {0}")]
    Apply(#[from] loa_core::ApplyError),
    #[error("request: {0}")]
    Request(#[from] loa_bus::RequestError),
    #[error("replication: {0}")]
    Replicate(#[from] loa_bus::ReplicateError),
}

/// Holder proof and revocation checks must occur before constructing this port.
/// The implementation rechecks expiry, revocation and scopes on every call;
/// `loa-grants::VerifiedGrant::visibility` supplies subtree selection.
pub trait Access: Send + Sync {
    fn allows(&self, peer: NodeKey, dom: &Dom, action: Action, node: Option<NodeId>) -> bool;
}

/// Read committed patches, including historical patches, without taking the
/// owner's writer lock. Must return exactly the requested contiguous range.
/// `Journal::patches(from)` is the disk implementation; the app serializes
/// reads with writes through its journal port when needed.
pub trait History: Send + Sync {
    fn read(&self, session: &SessionId, from: u64, through: u64) -> Result<Vec<Patch>, Error>;
}

/// A patch on the source owner's journal, optionally changing this peer's
/// projection. Even skipped patches advance `source_seq`, so reconnects resume
/// at an unambiguous position. Projected patches have their own gap-free seq.
#[derive(Clone, Debug, PartialEq)]
pub struct Update {
    pub source_seq: u64,
    pub patch: Option<Patch>,
}

/// Receives requests only on the owner; mirrors only receive ordered patches.
/// A request's base sequence is mandatory: stale concurrent edits are rejected
/// rather than silently applied against another user's changed state.
pub struct Owner<'a> {
    pub writer: &'a SessionWriter,
    pub access: &'a dyn Access,
}

impl Owner<'_> {
    pub fn request(&self, peer: NodeKey, base_seq: u64, draft: Draft, ts: i64) -> Result<Patch, Error> {
        if !self.writer.is_writer() {
            return Err(Error::NotOwner);
        }
        self.writer.with_dom(|dom| {
            if self.access.allows(peer, dom, Action::Drive, target(&draft)) {
                Ok(())
            } else {
                Err(Error::Denied(Action::Drive, self.writer.session().clone()))
            }
        })?;
        // The writer's Authorize port must recheck the verified grant against
        // its locked DOM. Both revision and authorization are checked atomically.
        match self.writer.request_if_seq(&peer, base_seq, draft, ts) {
            Err(loa_bus::RequestError::Conflict { expected, actual }) => {
                Err(Error::Conflict { expected, current: actual })
            }
            result => Ok(result?),
        }
    }

    pub fn stream(&self, peer: NodeKey) -> Result<Projection, Error> {
        if !self.writer.is_writer() { return Err(Error::NotOwner); }
        let allowed = self.writer.with_dom(|dom| self.access.allows(peer, dom, Action::View, None)
            || dom.iter().any(|n| n.kind == ElementKind::Turn && self.access.allows(peer, dom, Action::View, Some(n.id))));
        if !allowed { return Err(Error::Denied(Action::View, self.writer.session().clone())); }
        Ok(Projection::new(peer, self.writer.session().clone()))
    }
}

fn target(draft: &Draft) -> Option<NodeId> {
    match draft {
        Draft::Insert { parent, .. } => Some(*parent),
        Draft::Set { node, .. } | Draft::Append { node, .. } | Draft::Remove { node } => Some(*node),
    }
}

/// Filters the *source* journal in order. A projection starts at source seq 0;
/// use `catch_up` with the owner's history before forwarding live events.
/// On lag, discard the bus subscription, resubscribe and replay the missing
/// history. Never coalesce bus events for this protocol.
pub struct Projection {
    peer: NodeKey,
    session: SessionId,
    source: Dom,
    visible: Dom,
    ids: HashMap<NodeId, NodeId>,
}

impl Projection {
    pub fn new(peer: NodeKey, session: SessionId) -> Self {
        Self { peer, session, source: Dom::new(), visible: Dom::new(), ids: HashMap::new() }
    }

    pub fn source_seq(&self) -> u64 { self.source.seq() }
    pub fn visible_dom(&self) -> &Dom { &self.visible }

    /// The owner's id of the node the peer sees as `visible`. `None` when
    /// the peer can't see it.
    pub fn source_node(&self, visible: NodeId) -> Option<NodeId> {
        self.ids.iter().find(|(_, v)| **v == visible).map(|(s, _)| *s)
    }

    /// Rewrites a draft the peer made against its visible DOM into the owner's
    /// node ids. `None` when it names a node the peer can't see.
    pub fn source_draft(&self, draft: Draft) -> Option<Draft> {
        let source = |visible: NodeId| self.source_node(visible);
        Some(match draft {
            Draft::Insert { parent, index, kind, attrs, text } => {
                let parent_source = source(parent)?;
                // The peer counts only the children it sees; place the new
                // node after the source child it would follow.
                let index = match index {
                    None => None,
                    Some(0) => Some(0),
                    Some(i) => {
                        let visible_prev = *self.visible.get(parent)?.children.get(i as usize - 1)?;
                        let source_prev = source(visible_prev)?;
                        let position = self.source.get(parent_source)?.children.iter().position(|c| *c == source_prev)?;
                        Some(u32::try_from(position + 1).ok()?)
                    }
                };
                Draft::Insert { parent: parent_source, index, kind, attrs, text }
            }
            Draft::Set { node, name, value } => Draft::Set { node: source(node)?, name, value },
            Draft::Append { node, text } => Draft::Append { node: source(node)?, text },
            Draft::Remove { node } => Draft::Remove { node: source(node)? },
        })
    }

    /// A projection that starts at the owner's current DOM instead of patch 1,
    /// so the owner needs no journal history to serve a new subscriber. Returns
    /// the projection and the patches that build the peer's visible DOM; feed
    /// live patches after `source.seq()` to [`Projection::accept`].
    pub fn seeded(peer: NodeKey, session: SessionId, source: &Dom, access: &dyn Access) -> Result<(Self, Vec<Patch>), Error> {
        let mut projection = Self { peer, session, source: source.clone(), visible: Dom::new(), ids: HashMap::new() };
        let whole = access.allows(peer, source, Action::View, None);
        let mut patches = Vec::new();
        let ts = source.root().and_then(|root| root.int_attr(attr::CREATED)).unwrap_or_default();
        for node in source.iter() {
            let draft = if node.kind == ElementKind::Session {
                let attrs = node.attrs.iter()
                    .filter(|(name, _)| root_attr_visible(name, whole))
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                Draft::Insert { parent: NodeId::DOCUMENT, index: None, kind: node.kind, attrs, text: String::new() }
            } else {
                let Some(&parent) = projection.ids.get(&node.parent) else { continue };
                if !access.allows(peer, source, Action::View, Some(node.id)) { continue; }
                Draft::Insert { parent, index: None, kind: node.kind, attrs: node.attrs.clone(), text: node.text.clone() }
            };
            let patch = projection.visible.commit(draft, ts)?;
            projection.ids.insert(node.id, patch.node);
            patches.push(patch);
        }
        Ok((projection, patches))
    }

    pub fn accept(&mut self, source_patch: &Patch, access: &dyn Access) -> Result<Update, Error> {
        let removed_ids = if matches!(source_patch.op, Op::Remove) {
            self.source.descendants(source_patch.node).map(|node| node.id).collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let remove_allowed = matches!(source_patch.op, Op::Remove)
            && access.allows(self.peer, &self.source, Action::View, Some(source_patch.node));
        self.source.apply(source_patch)?;
        let old_visible = self.ids.contains_key(&source_patch.node);
        let allowed = remove_allowed
            || access.allows(self.peer, &self.source, Action::View, Some(source_patch.node));
        let update = match &source_patch.op {
            Op::Insert { parent, kind, attrs, text, .. } if allowed || *kind == ElementKind::Session => {
                let (parent, index, attrs, text) = if *kind == ElementKind::Session {
                    // A turn-only grant must not reveal project paths, digest,
                    // pre-existing context, or arbitrary root extension data.
                    let whole = access.allows(self.peer, &self.source, Action::View, None);
                    let safe = attrs.iter()
                        .filter(|(name, _)| root_attr_visible(name, whole))
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect();
                    (NodeId::DOCUMENT, None, safe, String::new())
                } else {
                    let Some(&visible_parent) = self.ids.get(parent) else {
                        return Ok(Update { source_seq: source_patch.seq, patch: None });
                    };
                    let preceding = self.source.children(*parent)
                        .take_while(|child| child.id != source_patch.node)
                        .filter(|child| self.ids.contains_key(&child.id))
                        .count() as u32;
                    (visible_parent, Some(preceding), attrs.clone(), text.clone())
                };
                let patch = self.visible.commit(Draft::Insert { parent, index, kind: *kind, attrs, text }, source_patch.ts)?;
                self.ids.insert(source_patch.node, patch.node);
                Some(patch)
            }
            Op::Set { name, value } if old_visible && self.source.root().is_some_and(|root| root.id == source_patch.node) => {
                let whole = access.allows(self.peer, &self.source, Action::View, None);
                if root_attr_visible(name, whole) {
                    Some(self.visible.commit(Draft::Set { node: self.ids[&source_patch.node], name: name.clone(), value: value.clone() }, source_patch.ts)?)
                } else {
                    None
                }
            }
            Op::Set { name, value } if allowed && old_visible => {
                Some(self.visible.commit(Draft::Set { node: self.ids[&source_patch.node], name: name.clone(), value: value.clone() }, source_patch.ts)?)
            }
            Op::Append { text } if allowed && old_visible => {
                Some(self.visible.commit(Draft::Append { node: self.ids[&source_patch.node], text: text.clone() }, source_patch.ts)?)
            }
            Op::Remove if old_visible => {
                let node = self.ids[&source_patch.node];
                for descendant in removed_ids { self.ids.remove(&descendant); }
                Some(self.visible.commit(Draft::Remove { node }, source_patch.ts)?)
            }
            _ => None,
        };
        Ok(Update { source_seq: source_patch.seq, patch: update })
    }

    pub fn catch_up(&mut self, through: u64, history: &dyn History, access: &dyn Access) -> Result<Vec<Update>, Error> {
        let from = self.source.seq() + 1;
        if through < from { return Ok(Vec::new()); }
        let patches = history.read(&self.session, from, through)?;
        let mut updates = Vec::with_capacity(patches.len());
        for patch in patches {
            updates.push(self.accept(&patch, access)?);
        }
        if self.source.seq() != through { return Err(Error::Sequence { last: self.source.seq(), got: through }); }
        Ok(updates)
    }
}

/// Root attributes a peer sees: identity and provenance always; the title only
/// with a grant on the whole session. Project paths and extension data stay
/// with the owner.
fn root_attr_visible(name: &str, whole_session: bool) -> bool {
    matches!(name, attr::ID | attr::SCHEMA | attr::HARNESS | attr::NODE | attr::CREATED)
        || (whole_session && name == attr::TITLE)
}

/// A mirror created with `SessionWriter` configured for the remote owner.
/// It starts from the owner's [`Seed`] and then takes every [`Update`] in
/// source order; a gap or a repeated packet is rejected, not applied. On
/// reconnect the caller starts a fresh mirror from a new seed.
pub struct Mirror {
    writer: Arc<SessionWriter>,
    source_seq: u64,
}

impl Mirror {
    pub fn new(writer: Arc<SessionWriter>, source_seq: u64) -> Result<Self, Error> {
        if writer.is_writer() { return Err(Error::NotOwner); }
        Ok(Self { writer, source_seq })
    }

    /// A mirror on an empty `writer` built from the owner's seed patches.
    pub fn seeded(writer: Arc<SessionWriter>, source_seq: u64, patches: Vec<Patch>) -> Result<Self, Error> {
        if writer.seq() != 0 {
            return Err(Error::Sequence { last: writer.seq(), got: 0 });
        }
        let mirror = Self::new(writer, source_seq)?;
        for patch in patches {
            mirror.writer.replicate(patch)?;
        }
        Ok(mirror)
    }

    pub fn source_seq(&self) -> u64 { self.source_seq }
    pub fn writer(&self) -> &Arc<SessionWriter> { &self.writer }

    pub fn receive(&mut self, update: Update) -> Result<(), Error> {
        if update.source_seq != self.source_seq + 1 {
            return Err(Error::Sequence { last: self.source_seq, got: update.source_seq });
        }
        if let Some(patch) = update.patch { self.writer.replicate(patch)?; }
        self.source_seq = update.source_seq;
        Ok(())
    }
}

/// Copies a peer's session, as this node mirrored it, into a new local
/// journal in `store` that this node owns. `patches` are the mirror's own
/// patches from seq 1 (what the peer's grant let this node see); the copy
/// keeps those up to `at_seq` and then sets the root's `id`, `parent_session`,
/// `forked_at` and `node`. The caller must first check `Action::Fork`. The app
/// starts its harness on the new journal with this node's credentials, never
/// the remote owner's.
pub fn fork_mirror_to_own_node(store: &Store, patches: &[Patch], at_seq: u64, new_id: &str, local_node: &str) -> Result<Journal, Error> {
    let built = (|| {
        let mut journal = store.create(new_id)?;
        let mut dom = Dom::new();
        for patch in patches.iter().take_while(|patch| patch.seq <= at_seq) {
            dom.apply(patch)?;
            journal.append(patch)?;
        }
        if dom.seq() != at_seq {
            return Err(Error::Sequence { last: dom.seq(), got: at_seq });
        }
        let root = dom.root().ok_or(Error::NoSession)?;
        let (root, source) = (root.id, root.str_attr(attr::ID).unwrap_or_default().to_owned());
        let ts = loa_core::now_ms();
        let drafts = [
            Draft::set(root, attr::ID, new_id),
            Draft::set(root, attr::PARENT_SESSION, source.as_str()),
            Draft::set(root, attr::FORKED_AT, Value::Int(i64::try_from(at_seq).unwrap_or(i64::MAX))),
            Draft::set(root, attr::NODE, local_node),
        ];
        for draft in drafts {
            let patch = dom.commit(draft, ts)?;
            journal.append_applied(&patch, &dom)?;
        }
        journal.write_snapshot(&dom)?;
        journal.sync()?;
        Ok(journal)
    })();
    if built.is_err() {
        // Leave no half made session behind; a store that refuses the id
        // (already present, invalid) is left untouched by `create` itself.
        if !matches!(built, Err(Error::Journal(loa_journal::Error::InvalidSessionId(_) | loa_journal::Error::Exists(_)))) {
            let _ = store.delete(new_id);
        }
    }
    built
}

/// Wire envelope for `omp/session/1`. Every message is a length-delimited
/// protobuf frame; the authenticated transport supplies the peer key. This
/// codec does not trust a node key supplied by the remote payload.
///
/// Each stream carries one request. The requester opens the stream with a
/// request ([`ListSessions`], [`Subscribe`], [`Drive`], [`Prompt`] or
/// [`Answer`]; the last three also travel on a subscription stream); the
/// owner answers with a [`Challenge`]; the requester signs it over each grant
/// token it holds from the owner ([`Present`]); the owner checks the grants
/// and answers: [`SessionList`], a [`Seed`] followed by [`WireUpdate`]s,
/// [`DriveResult`], or [`Ack`]. A refused request gets an [`Ack`] with an error.
#[derive(Clone, PartialEq, Message)]
pub struct Frame {
    #[prost(oneof = "frame::Body", tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12")]
    pub body: Option<frame::Body>,
}

pub mod frame {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Body {
        #[prost(message, tag = "1")]
        Subscribe(super::Subscribe),
        #[prost(message, tag = "2")]
        Update(super::WireUpdate),
        #[prost(message, tag = "3")]
        Drive(super::Drive),
        #[prost(message, tag = "4")]
        Challenge(super::Challenge),
        #[prost(message, tag = "5")]
        Present(super::Present),
        #[prost(message, tag = "6")]
        ListSessions(super::ListSessions),
        #[prost(message, tag = "7")]
        SessionList(super::SessionList),
        #[prost(message, tag = "8")]
        Seed(super::Seed),
        #[prost(message, tag = "9")]
        DriveResult(super::DriveResult),
        #[prost(message, tag = "10")]
        Prompt(super::Prompt),
        #[prost(message, tag = "11")]
        Ack(super::Ack),
        #[prost(message, tag = "12")]
        Answer(super::Answer),
    }
}

impl From<frame::Body> for Frame {
    fn from(body: frame::Body) -> Self {
        Self { body: Some(body) }
    }
}

/// A fresh nonce the requester signs to prove it holds its grants' keys.
#[derive(Clone, PartialEq, Message)]
pub struct Challenge {
    #[prost(bytes = "vec", tag = "1")]
    pub nonce: Vec<u8>,
}

/// One grant token and the holder's signature over the challenge and token.
#[derive(Clone, PartialEq, Message)]
pub struct PresentedGrant {
    #[prost(bytes = "vec", tag = "1")]
    pub token: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub proof: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Present {
    #[prost(message, repeated, tag = "1")]
    pub grants: Vec<PresentedGrant>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ListSessions {}

/// A session the requester's grants reach, with what they allow on it.
#[derive(Clone, PartialEq, Message)]
pub struct SessionEntry {
    #[prost(string, tag = "1")]
    pub id: String,
    /// Empty unless the grant covers the whole session.
    #[prost(string, tag = "2")]
    pub title: String,
    #[prost(string, tag = "3")]
    pub harness: String,
    #[prost(int64, tag = "4")]
    pub created_ms: i64,
    /// Actions allowed on the whole session (`view`, `suggest`, `drive`, `fork`).
    #[prost(string, repeated, tag = "5")]
    pub actions: Vec<String>,
    /// Some prompts are visible without the whole session.
    #[prost(bool, tag = "6")]
    pub partial: bool,
    #[prost(uint64, tag = "7")]
    pub last_seq: u64,
}

#[derive(Clone, PartialEq, Message)]
pub struct SessionList {
    #[prost(message, repeated, tag = "1")]
    pub sessions: Vec<SessionEntry>,
}

/// The requester's visible DOM at the owner's `source_seq`, as patches from 1.
#[derive(Clone, PartialEq, Message)]
pub struct Seed {
    #[prost(uint64, tag = "1")]
    pub source_seq: u64,
    #[prost(message, repeated, tag = "2")]
    pub patches: Vec<loa_journal::proto::Patch>,
    /// Actions the requester's grants allow on the whole session.
    #[prost(string, repeated, tag = "3")]
    pub actions: Vec<String>,
}

impl Seed {
    pub fn new(source_seq: u64, patches: &[Patch], actions: &[Action]) -> Self {
        Self {
            source_seq,
            patches: patches.iter().map(Into::into).collect(),
            actions: actions.iter().map(|a| a.as_str().to_owned()).collect(),
        }
    }

    pub fn patches(self) -> Result<Vec<Patch>, Error> {
        self.patches.into_iter().map(TryInto::try_into).collect::<Result<_, loa_journal::proto::ProtoError>>()
            .map_err(|e| Error::Wire(e.to_string()))
    }
}

/// The owner's answer to a [`Drive`]: the committed patch, or why not.
#[derive(Clone, PartialEq, Message)]
pub struct DriveResult {
    #[prost(message, optional, tag = "1")]
    pub patch: Option<loa_journal::proto::Patch>,
    /// The owner's sequence when the request's base was stale.
    #[prost(uint64, optional, tag = "2")]
    pub conflict_current: Option<u64>,
    #[prost(string, tag = "3")]
    pub error: String,
}

/// A prompt for the owner's harness, from a peer allowed to drive the session.
#[derive(Clone, PartialEq, Message)]
pub struct Prompt {
    #[prost(string, tag = "1")]
    pub session: String,
    #[prost(string, tag = "2")]
    pub text: String,
}

/// A peer's answer to a waiting decision, for the owner to hand to the
/// harness that waits on it (needs drive on the decision).
#[derive(Clone, PartialEq, Message)]
pub struct Answer {
    #[prost(string, tag = "1")]
    pub session: String,
    /// The decision's node id in the peer's view.
    #[prost(uint64, tag = "2")]
    pub decision: u64,
    /// Picked options, typed text, or a constant from `loa_core::answer`.
    #[prost(string, repeated, tag = "3")]
    pub values: Vec<String>,
    /// Deny reason, reject feedback, or the edited plan.
    #[prost(string, optional, tag = "4")]
    pub reason: Option<String>,
}

/// Accepted when `error` is empty.
#[derive(Clone, PartialEq, Message)]
pub struct Ack {
    #[prost(string, tag = "1")]
    pub error: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct Subscribe {
    #[prost(string, tag = "1")]
    pub session: String,
    #[prost(uint64, tag = "2")]
    pub after_source_seq: u64,
}

#[derive(Clone, PartialEq, Message)]
pub struct WireUpdate {
    #[prost(uint64, tag = "1")]
    pub source_seq: u64,
    #[prost(message, optional, tag = "2")]
    pub patch: Option<loa_journal::proto::Patch>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Drive {
    #[prost(string, tag = "1")]
    pub session: String,
    #[prost(uint64, tag = "2")]
    pub base_seq: u64,
    #[prost(bytes = "vec", tag = "3")]
    pub draft: Vec<u8>,
    #[prost(int64, tag = "4")]
    pub ts: i64,
}

impl From<&Update> for WireUpdate {
    fn from(update: &Update) -> Self {
        Self { source_seq: update.source_seq, patch: update.patch.as_ref().map(Into::into) }
    }
}

impl TryFrom<WireUpdate> for Update {
    type Error = Error;
    fn try_from(wire: WireUpdate) -> Result<Self, Error> {
        Ok(Self { source_seq: wire.source_seq, patch: wire.patch.map(TryInto::try_into).transpose().map_err(|e: loa_journal::proto::ProtoError| Error::Wire(e.to_string()))? })
    }
}

impl Drive {
    pub fn new(session: &str, base_seq: u64, draft: &Draft, ts: i64) -> Result<Self, Error> {
        let draft = serde_json::to_vec(draft).map_err(|e| Error::Wire(e.to_string()))?;
        Ok(Self { session: session.to_owned(), base_seq, draft, ts })
    }
    pub fn draft(&self) -> Result<Draft, Error> {
        serde_json::from_slice(&self.draft).map_err(|e| Error::Wire(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TurnAccess(NodeId);

    impl Access for TurnAccess {
        fn allows(&self, _: NodeKey, dom: &Dom, action: Action, node: Option<NodeId>) -> bool {
            action == Action::View && node.is_some_and(|id| {
                id == self.0 || dom.ancestors(id).any(|ancestor| ancestor.id == self.0)
            })
        }
    }

    #[test]
    fn projection_hides_other_turns_and_keeps_visible_order() {
        let peer = NodeKey([1; 32]);
        let mut source = Dom::new();
        let root = source.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, [
            (attr::ID, Value::from("session")), (attr::HARNESS, Value::from("omp")),
            (attr::NODE, Value::from("owner")), (attr::CREATED, Value::Int(1)),
            (attr::PROJECT, Value::from("private")),
        ], ""), 1).unwrap();
        let hidden = source.commit(Draft::insert(root.node, ElementKind::Turn, std::iter::empty::<(&str, Value)>(), ""), 2).unwrap();
        let visible = source.commit(Draft::insert(root.node, ElementKind::Turn, std::iter::empty::<(&str, Value)>(), ""), 3).unwrap();
        let first = source.commit(Draft::insert(visible.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], "first"), 4).unwrap();
        let second = source.commit(Draft::insert(visible.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], "second"), 5).unwrap();
        let secret = source.commit(Draft::append(hidden.node, "secret"), 6).unwrap();
        let mut projection = Projection::new(peer, SessionId::from("session"));
        let access = TurnAccess(visible.node);
        let updates: Vec<_> = [&root, &hidden, &visible, &first, &second, &secret]
            .into_iter().map(|patch| projection.accept(patch, &access).unwrap()).collect();
        assert_eq!(updates.iter().map(|u| u.source_seq).collect::<Vec<_>>(), (1..=6).collect::<Vec<_>>());
        assert!(updates[1].patch.is_none());
        assert!(updates[5].patch.is_none());
        let root = projection.visible_dom().root().unwrap();
        assert!(!root.attrs.contains_key(attr::PROJECT));
        assert_eq!(root.children.len(), 1);
        let turn = projection.visible_dom().get(root.children[0]).unwrap();
        assert_eq!(turn.children.len(), 2);
        assert_eq!(projection.visible_dom().get(turn.children[0]).unwrap().text, "first");
        assert_eq!(projection.visible_dom().get(turn.children[1]).unwrap().text, "second");
        assert_eq!(projection.visible_dom().seq(), 4);
    }

    #[test]
    fn projection_rejects_missing_source_patch() {
        let mut projection = Projection::new(NodeKey([1; 32]), SessionId::from("session"));
        let patch = Patch { seq: 2, node: NodeId(1), ts: 0, op: Op::Remove };
        assert!(matches!(projection.accept(&patch, &TurnAccess(NodeId(1))), Err(Error::Apply(loa_core::ApplyError::Sequence { .. }))));
        assert_eq!(projection.source_seq(), 0);
    }

    struct WholeAccess;
    impl Access for WholeAccess {
        fn allows(&self, _: NodeKey, _: &Dom, action: Action, _: Option<NodeId>) -> bool { action == Action::View }
    }

    fn source_session() -> (Dom, Vec<Patch>, NodeId) {
        let mut source = Dom::new();
        let mut patches = Vec::new();
        let root = source.commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, [
            (attr::ID, Value::from("session")), (attr::HARNESS, Value::from("omp")),
            (attr::NODE, Value::from("owner")), (attr::CREATED, Value::Int(1)),
            (attr::PROJECT, Value::from("private")),
        ], ""), 1).unwrap();
        patches.push(root.clone());
        patches.push(source.commit(Draft::set(root.node, attr::TITLE, "Plan"), 2).unwrap());
        let hidden = source.commit(Draft::insert(root.node, ElementKind::Turn, std::iter::empty::<(&str, Value)>(), ""), 3).unwrap();
        let visible = source.commit(Draft::insert(root.node, ElementKind::Turn, std::iter::empty::<(&str, Value)>(), ""), 4).unwrap();
        patches.extend([hidden.clone(), visible.clone()]);
        let message = source.commit(Draft::insert(visible.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], "hel"), 5).unwrap();
        patches.push(message.clone());
        patches.push(source.commit(Draft::append(message.node, "lo"), 6).unwrap());
        patches.push(source.commit(Draft::insert(hidden.node, ElementKind::Message, [(attr::ROLE, Value::from("user"))], "secret"), 7).unwrap());
        (source, patches, visible.node)
    }

    /// Every node's kind, text and attributes, in document order.
    type Shape = Vec<(ElementKind, String, Vec<(String, String)>)>;

    fn shape(dom: &Dom) -> Shape {
        dom.iter().map(|n| (n.kind, n.text.clone(), n.attrs.iter().map(|(k, v)| (k.clone(), format!("{v:?}"))).collect())).collect()
    }

    #[test]
    fn seeded_projection_matches_replaying_every_patch() {
        let (source, patches, visible) = source_session();
        let peer = NodeKey([1; 32]);
        for access in [&TurnAccess(visible) as &dyn Access, &WholeAccess] {
            let mut replayed = Projection::new(peer, SessionId::from("session"));
            for patch in &patches { replayed.accept(patch, access).unwrap(); }
            let (seeded, seed) = Projection::seeded(peer, SessionId::from("session"), &source, access).unwrap();
            assert_eq!(seeded.source_seq(), source.seq());
            assert_eq!(shape(seeded.visible_dom()), shape(replayed.visible_dom()));
            let mut mirror = Dom::new();
            for patch in &seed { mirror.apply(patch).unwrap(); }
            assert_eq!(shape(&mirror), shape(seeded.visible_dom()));
        }
        let (seeded, _) = Projection::seeded(peer, SessionId::from("session"), &source, &TurnAccess(visible)).unwrap();
        let root = seeded.visible_dom().root().unwrap();
        assert!(!root.attrs.contains_key(attr::PROJECT));
        assert!(!root.attrs.contains_key(attr::TITLE), "a prompt grant does not reveal the session title");
        let (whole, _) = Projection::seeded(peer, SessionId::from("session"), &source, &WholeAccess).unwrap();
        assert_eq!(whole.visible_dom().root().unwrap().str_attr(attr::TITLE), Some("Plan"));
        assert!(!whole.visible_dom().root().unwrap().attrs.contains_key(attr::PROJECT));
    }

    #[test]
    fn mirror_fork_becomes_a_local_session_owned_by_this_node() {
        let (source, _, _) = source_session();
        let (_, seed) = Projection::seeded(NodeKey([1; 32]), SessionId::from("session"), &source, &WholeAccess).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path(), loa_journal::JournalOptions::default()).unwrap();
        let journal = fork_mirror_to_own_node(&store, &seed, seed.len() as u64, "forked", "me").unwrap();
        let dom = journal.load().unwrap();
        let root = dom.root().unwrap();
        assert_eq!(root.str_attr(attr::ID), Some("forked"));
        assert_eq!(root.str_attr(attr::NODE), Some("me"));
        assert_eq!(root.str_attr(attr::PARENT_SESSION), Some("session"));
        assert_eq!(dom.iter().filter(|n| n.kind == ElementKind::Message).count(), 2);
        drop(journal);
        assert!(matches!(fork_mirror_to_own_node(&store, &seed, 99, "short", "me"), Err(Error::Sequence { .. })));
        assert_eq!(store.list().unwrap().len(), 1, "a failed fork leaves nothing behind");
        assert!(matches!(fork_mirror_to_own_node(&store, &seed, 1, "forked", "me"), Err(Error::Journal(loa_journal::Error::Exists(_)))));
    }
}
