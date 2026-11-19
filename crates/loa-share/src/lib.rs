//! Owner-ordered patch sharing. The app binds the ports here to the authenticated
//! `omp/session/1` connection, journal reader, and verified Biscuit grants.
//!
//! A receiver never assigns a sequence number to a remote session. Requests go
//! to the owner; only committed patches come back. A partial grant receives an
//! independently numbered projection, never the owner's hidden patches.

use loa_bus::{NodeKey, SessionId, SessionWriter};
use loa_core::{attr, Dom, Draft, ElementKind, NodeId, Op, Patch, Value};
use loa_grants::Action;
use loa_journal::Journal;
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
                    let mut safe = loa_core::Attrs::new();
                    for key in [attr::ID, attr::SCHEMA, attr::HARNESS, attr::NODE, attr::CREATED] {
                        if let Some(value) = attrs.get(key) { safe.insert(key.to_owned(), value.clone()); }
                    }
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
            Op::Set { name, value } if allowed && old_visible && source_patch.node != self.source.root().map_or(NodeId::DOCUMENT, |n| n.id) => {
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

/// A mirror created with `SessionWriter` configured for the remote owner.
/// `source_seq` is persisted by the caller alongside the mirror's journal
/// checkpoint; on reconnect, resubscribe at source_seq + 1. Repeating a packet
/// after an acknowledged checkpoint is rejected, not applied twice.
pub struct Mirror {
    writer: Arc<SessionWriter>,
    source_seq: u64,
}

impl Mirror {
    pub fn new(writer: Arc<SessionWriter>, source_seq: u64) -> Result<Self, Error> {
        if writer.is_writer() { return Err(Error::NotOwner); }
        Ok(Self { writer, source_seq })
    }
    pub fn source_seq(&self) -> u64 { self.source_seq }
    pub fn receive(&mut self, update: Update) -> Result<(), Error> {
        if update.source_seq != self.source_seq + 1 {
            return Err(Error::Sequence { last: self.source_seq, got: update.source_seq });
        }
        if let Some(patch) = update.patch { self.writer.replicate(patch)?; }
        self.source_seq = update.source_seq;
        Ok(())
    }
}

/// The caller must first verify `Action::Fork` for this peer against the owner.
/// Forks retain only the already authorized local projection. Ownership is
/// changed *in the new journal*; its next sequence follows the copied source
/// patches, with new IDs never reused. The app starts its harness on this new
/// journal using the local node's credentials, never the remote credentials.
pub fn fork_to_own_node(source: &Journal, at_seq: u64, new_id: &str, local_node: &str) -> Result<Journal, Error> {
    let mut fork = source.fork(at_seq, new_id)?;
    let mut dom = fork.load()?;
    let root = dom.root().ok_or(Error::NoSession)?.id;
    let patch = dom.commit(Draft::set(root, attr::NODE, Value::Str(local_node.to_owned())), loa_core::now_ms())?;
    fork.append_applied(&patch, &dom)?;
    fork.sync()?;
    Ok(fork)
}

/// Wire envelope for `omp/session/1`. Every message is a length-delimited
/// protobuf frame; the authenticated transport supplies the peer key. This
/// codec does not trust a node key supplied by the remote payload.
#[derive(Clone, PartialEq, Message)]
pub struct Frame {
    #[prost(oneof = "frame::Body", tags = "1, 2, 3")]
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
    }
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
}
