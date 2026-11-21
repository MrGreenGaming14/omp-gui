//! Grants on this node: the [`GrantGate`] every protocol checks, the shared
//! challenge/present handshake, and [`GrantService`] over the grant store.

use crate::protocols::projection::Access;
use crate::transport::Stream;
use crate::wire::{self, Body, Challenge as WireChallenge, Frame, Present, PresentedGrant};
use crate::{NodeError, NodeId, Result};
use loa_bus::{Authorize, Denied, NodeKey, SessionId};
use loa_core::{Dom, Draft, NodeId as DomNodeId};
use loa_trust::{Action, Challenge, GrantId, GrantStore, NodeIdentity, Presented, Proof, PublicKey, Request, Resource, Revocations, Scope};
use loa_trust::{SessionFacts, VerifiedGrant, Verifier, prove, turn_of};
use loa_util::LockExt;
use loa_util::time::now_ms;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Port: the grants a peer proved that still count, and what they allow.
pub trait GrantGate: Send + Sync {
    /// Proven grants that are neither expired nor revoked at `now`.
    fn live_grants(&self, peer: &NodeKey, now: i64) -> Vec<VerifiedGrant>;
    fn allows(&self, peer: &NodeKey, request: &Request<'_>, now: i64) -> bool {
        self.live_grants(peer, now).iter().any(|grant| grant.authorize(request, now).is_ok())
    }
}

/// The session actions a grant can carry, strongest last.
const SESSION_ACTIONS: [Action; 4] = [Action::View, Action::Fork, Action::Suggest, Action::Drive];

/// The grants each peer last proved, checked again on every use. It backs
/// the session [`Access`] and the [`Authorize`] of every session writer.
pub struct GrantAccess {
    store: Arc<GrantStore>,
    held: Mutex<HashMap<NodeKey, Vec<VerifiedGrant>>>,
}

impl GrantGate for GrantAccess {
    fn live_grants(&self, peer: &NodeKey, now: i64) -> Vec<VerifiedGrant> {
        let held = self.held.locked();
        let live = |g: &&VerifiedGrant| g.expires_ms() > now && !g.chain().iter().any(|link| self.store.is_revoked(&link.grant));
        held.get(peer).into_iter().flatten().filter(live).cloned().collect()
    }
}

impl GrantAccess {
    pub fn new(store: Arc<GrantStore>) -> Self { Self { store, held: Mutex::default() } }

    /// Records the grants `peer` just proved, replacing earlier ones.
    pub fn set(&self, peer: NodeKey, grants: Vec<VerifiedGrant>) { self.held.locked().insert(peer, grants); }

    pub fn session_actions(&self, peer: &NodeKey, facts: &SessionFacts) -> Vec<Action> {
        let now = now_ms();
        SESSION_ACTIONS.into_iter().filter(|action| GrantGate::allows(self, peer, &Request::session(*action, facts), now)).collect()
    }

    pub fn turn_visible(&self, peer: &NodeKey, facts: &SessionFacts, turn: DomNodeId) -> bool {
        GrantGate::allows(self, peer, &Request::turn(Action::View, facts, turn), now_ms())
    }

    pub(crate) fn allows_in(&self, peer: &NodeKey, dom: &Dom, action: Action, node: Option<DomNodeId>) -> bool {
        let Some(facts) = SessionFacts::from_dom(dom) else { return false };
        let (now, turn) = (now_ms(), node.and_then(|node| turn_of(dom, node)));
        self.live_grants(peer, now).iter().any(|grant| {
            grant.authorize(&Request::session(action, &facts), now).is_ok()
                || turn.is_some_and(|turn| grant.authorize(&Request::turn(action, &facts, turn), now).is_ok())
        })
    }
}

impl Access for GrantAccess {
    fn allows(&self, peer: NodeKey, dom: &Dom, action: Action, node: Option<DomNodeId>) -> bool { self.allows_in(&peer, dom, action, node) }
}

/// The element a draft acts on.
pub(crate) fn target(draft: &Draft) -> DomNodeId {
    match draft {
        Draft::Insert { parent, .. } => *parent,
        Draft::Set { node, .. } | Draft::Append { node, .. } | Draft::Remove { node } => *node,
    }
}

impl Authorize for GrantAccess {
    fn authorize(&self, session: &SessionId, requester: &NodeKey, draft: &Draft, dom: &Dom) -> Result<(), Denied> {
        if self.allows_in(requester, dom, Action::Drive, Some(target(draft))) { Ok(()) } else { Err(Denied(format!("no drive grant on {session} for this peer"))) }
    }
}

/// The `Authorize` of mirrors, which take no requests.
pub(crate) struct Refuse;

impl Authorize for Refuse {
    fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> { Err(Denied("this node does not own the session".into())) }
}

/// Whether a grant was given by this node or is held from a peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantDirection {
    Given,
    Received,
}

/// A grant this node issued or holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantInfo {
    pub id: GrantId,
    /// The holder of a given grant, the issuer of a received one.
    pub peer: NodeId,
    pub direction: GrantDirection,
    pub scopes: Vec<Scope>,
    /// When a given grant ends.
    pub until_ms: Option<i64>,
    /// When a received grant's token expires, and when it renews.
    pub expires_ms: Option<i64>,
    pub renews_at_ms: Option<i64>,
    pub reshare: bool,
    /// Revoked here, or refused renewal by its issuer.
    pub ended: bool,
}

impl GrantInfo {
    /// A received grant that has neither ended nor expired.
    pub fn live_received(&self, now: i64) -> bool {
        self.direction == GrantDirection::Received && !self.ended && self.expires_ms.is_none_or(|expires| expires > now)
    }
}

/// The grant store with this node's identity: issuing, receiving and proving.
pub struct GrantService {
    pub store: Arc<GrantStore>,
    pub access: Arc<GrantAccess>,
    pub identity: NodeIdentity,
    pub renew_margin_ms: i64,
    /// Grants issued while their holder was unreachable.
    pub undelivered: Mutex<HashSet<GrantId>>,
}

impl GrantService {
    pub fn new(store: Arc<GrantStore>, identity: NodeIdentity, renew_margin_ms: i64) -> Self {
        Self { access: Arc::new(GrantAccess::new(Arc::clone(&store))), store, identity, renew_margin_ms, undelivered: Mutex::default() }
    }

    pub fn key(&self) -> PublicKey { self.identity.public_key() }

    /// Owner side of the handshake: challenges `peer` and records the grants it proves.
    pub async fn challenge(&self, peer: NodeId, stream: &mut Stream) -> Result<()> {
        let challenge = Challenge::random()?;
        let reply: Frame = wire::call(stream, &Frame::from(Body::Challenge(WireChallenge { nonce: challenge.0.to_vec() }))).await?;
        let Some(Body::Present(present)) = reply.body else { return Err(NodeError::Protocol("expected the requester's grants".into())) };
        let (requester, verifier, now) = (PublicKey::from(peer), Verifier::new(self.key(), &*self.store), now_ms());
        let proven = present.grants.iter().filter_map(|grant| {
            let proof = Proof(grant.proof.as_slice().try_into().ok()?);
            verifier.verify(Presented { token: &grant.token, proof: &proof }, &requester, &challenge, now)
                .inspect_err(|denied| tracing::debug!(%peer, %denied, "presented grant refused")).ok()
        });
        self.access.set(requester.into(), proven.collect());
        Ok(())
    }

    /// Requester side: answers `peer`'s challenge with every live grant held from it.
    pub async fn present(&self, peer: NodeId, stream: &mut Stream) -> Result<()> {
        let Some(Body::Challenge(challenge)) = wire::expect::<Frame>(&mut stream.recv).await?.body else {
            return Err(NodeError::Protocol("expected a challenge".into()));
        };
        let challenge = Challenge(challenge.nonce.as_slice().try_into().map_err(|_| NodeError::Protocol("challenge is not 32 bytes".into()))?);
        let (root, now) = (PublicKey::from(peer), now_ms());
        let grants = self.store.received_all()?.into_iter().filter(|g| g.root == root && g.refused_ms.is_none() && g.expires_ms > now);
        let grants = grants.map(|g| PresentedGrant { proof: prove(&self.identity, &challenge, &g.token).0.to_vec(), token: g.token }).collect();
        wire::send(&mut stream.send, &Frame::from(Body::Present(Present { grants }))).await
    }

    /// Whether `peer` proved a live grant for `action` on `resource` (sandbox, SSH, surface or folder).
    pub fn covers(&self, peer: NodeId, resource: &Resource, action: Action) -> bool {
        let request = match resource {
            Resource::Sandbox => Request::sandbox(action),
            Resource::Ssh { host } => Request::ssh(action, host),
            Resource::Surface { uri } => Request::surface(action, uri),
            Resource::Folder { path } => Request::folder(action, path),
            _ => return false,
        };
        GrantGate::allows(&*self.access, &PublicKey::from(peer).into(), &request, now_ms())
    }

    /// Grants issued here, then grants held.
    pub fn infos(&self) -> Vec<GrantInfo> {
        let issued = self.store.issued().unwrap_or_default().into_iter().filter_map(|r| {
            Some(GrantInfo {
                peer: NodeId::try_from(r.holder).ok()?,
                id: r.id,
                direction: GrantDirection::Given,
                scopes: r.scopes.unwrap_or_default(),
                until_ms: r.until_ms,
                expires_ms: None,
                renews_at_ms: None,
                reshare: r.reshare,
                ended: r.revoked_ms.is_some(),
            })
        });
        let received = self.store.received_all().unwrap_or_default().into_iter().filter_map(|r| {
            Some(GrantInfo {
                peer: NodeId::try_from(r.issuer).ok()?,
                scopes: r.scopes().unwrap_or_default(),
                id: r.id,
                direction: GrantDirection::Received,
                until_ms: None,
                expires_ms: Some(r.expires_ms),
                renews_at_ms: r.refused_ms.is_none().then(|| r.expires_ms - self.renew_margin_ms),
                reshare: r.reshare,
                ended: r.refused_ms.is_some(),
            })
        });
        issued.chain(received).collect()
    }

    /// Whether this node holds a grant from `peer` it still renews.
    pub fn holds_from(&self, peer: &NodeId) -> bool {
        let root = PublicKey::from(*peer);
        self.store.received_all().unwrap_or_default().iter().any(|g| g.root == root && g.refused_ms.is_none())
    }

    /// Stores a grant `sender` delivered; only its issuer may deliver it.
    pub fn receive(&self, sender: NodeId, token: &[u8], root: &[u8]) -> Result<()> {
        let root = match root {
            [] => PublicKey::from(sender),
            bytes => PublicKey(bytes.try_into().map_err(|_| NodeError::Protocol("root key is not 32 bytes".into()))?),
        };
        let received = self.store.receive(&self.key(), &root, token)?;
        if received.issuer != PublicKey::from(sender) {
            return Err(NodeError::Refused("only a grant's issuer may deliver it".into()));
        }
        Ok(())
    }

    /// Ends a held grant on its issuer's notice.
    pub fn revoked_by(&self, sender: NodeId, id: &GrantId) -> Result<()> {
        let Some(held) = self.store.received(id)? else { return Ok(()) };
        if held.issuer != PublicKey::from(sender) {
            return Err(NodeError::Refused("only a grant's issuer may revoke it".into()));
        }
        self.store.record_revocation(id, &held.issuer, now_ms())?;
        Ok(self.store.mark_refused(id, now_ms())?)
    }
}
