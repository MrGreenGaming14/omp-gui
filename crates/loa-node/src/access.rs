//! What each peer may do here, from the grants it presented.

use loa_util::LockExt;
use loa_bus::{Authorize, Denied, NodeKey, SessionId};
use loa_core::{Dom, Draft, NodeId, now_ms};
use loa_trust::{Action, GrantStore, Request, Revocations, SessionFacts, VerifiedGrant, turn_of};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The session actions a grant can carry, strongest last.
const SESSION_ACTIONS: [Action; 4] = [Action::View, Action::Fork, Action::Suggest, Action::Drive];

/// The grants each peer last proved it holds, checked again on every use:
/// a revoked or expired grant stops counting at once.
///
/// One instance backs both ports that guard this node's sessions: the
/// [`loa_share::Access`] check of the session protocol and the
/// [`loa_bus::Authorize`] of every session writer the app opens, so a peer's
/// drive request is checked against the locked DOM it applies to.
pub struct GrantAccess {
    store: Arc<GrantStore>,
    held: Mutex<HashMap<NodeKey, Vec<VerifiedGrant>>>,
}

impl GrantAccess {
    pub fn new(store: Arc<GrantStore>) -> Self {
        Self { store, held: Mutex::default() }
    }

    /// Records the grants `peer` just proved, replacing what it proved before.
    pub fn set(&self, peer: NodeKey, grants: Vec<VerifiedGrant>) {
        self.held.locked().insert(peer, grants);
    }

    fn live(&self, peer: &NodeKey, now: i64) -> Vec<VerifiedGrant> {
        let held = self.held.locked();
        held.get(peer)
            .into_iter()
            .flatten()
            .filter(|grant| grant.expires_ms() > now && !grant.chain().iter().any(|link| self.store.is_revoked(&link.grant)))
            .cloned()
            .collect()
    }

    /// Whether `peer` still holds any grant that is neither revoked nor expired.
    pub fn has_live(&self, peer: &NodeKey) -> bool {
        !self.live(peer, now_ms()).is_empty()
    }

    /// Actions `peer` may take on the whole session.
    pub fn session_actions(&self, peer: &NodeKey, facts: &SessionFacts) -> Vec<Action> {
        let now = now_ms();
        let grants = self.live(peer, now);
        SESSION_ACTIONS
            .into_iter()
            .filter(|action| grants.iter().any(|g| g.authorize(&Request::session(*action, facts), now).is_ok()))
            .collect()
    }

    /// Whether `peer` may see prompt `turn` of the session.
    pub fn turn_visible(&self, peer: &NodeKey, facts: &SessionFacts, turn: NodeId) -> bool {
        let now = now_ms();
        self.live(peer, now).iter().any(|g| g.authorize(&Request::turn(Action::View, facts, turn), now).is_ok())
    }

    fn allows_in(&self, peer: &NodeKey, dom: &Dom, action: Action, node: Option<NodeId>) -> bool {
        let Some(facts) = SessionFacts::from_dom(dom) else { return false };
        let now = now_ms();
        let grants = self.live(peer, now);
        grants.iter().any(|grant| {
            grant.authorize(&Request::session(action, &facts), now).is_ok()
                || node
                    .and_then(|node| turn_of(dom, node))
                    .is_some_and(|turn| grant.authorize(&Request::turn(action, &facts, turn), now).is_ok())
        })
    }
}

impl loa_share::Access for GrantAccess {
    fn allows(&self, peer: NodeKey, dom: &Dom, action: Action, node: Option<NodeId>) -> bool {
        self.allows_in(&peer, dom, action, node)
    }
}

impl Authorize for GrantAccess {
    fn authorize(&self, session: &SessionId, requester: &NodeKey, draft: &Draft, dom: &Dom) -> Result<(), Denied> {
        let target = match draft {
            Draft::Insert { parent, .. } => *parent,
            Draft::Set { node, .. } | Draft::Append { node, .. } | Draft::Remove { node } => *node,
        };
        if self.allows_in(requester, dom, Action::Drive, Some(target)) {
            Ok(())
        } else {
            Err(Denied(format!("no drive grant on {session} for this peer")))
        }
    }
}

/// Refuses every peer request; the `Authorize` of mirrors, which take no requests.
pub(crate) struct Refuse;

impl Authorize for Refuse {
    fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
        Err(Denied("this node does not own the session".into()))
    }
}
