//! What a grant covers and what a peer asks for.

use crate::{GrantError, Result};
use loa_core::{Dom, ElementKind, NodeId, attr};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One permission. Sessions (and prompts, projects, future sessions) take
/// view, suggest, drive and fork; surfaces take view and drive; SSH hosts take
/// read, write and exec.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Read the session or watch the surface.
    View,
    /// Send prompts or edits the owner applies after review.
    Suggest,
    /// Prompt the session directly, answer its decisions, use its tools; or
    /// send input to a surface.
    Drive,
    /// Copy the session and run it on the peer's own machine and keys.
    Fork,
    /// SSH: read files on the host.
    Read,
    /// SSH: write files on the host.
    Write,
    /// SSH: run commands on the host.
    Exec,
}

impl Action {
    pub const fn as_str(self) -> &'static str {
        match self {
            Action::View => "view",
            Action::Suggest => "suggest",
            Action::Drive => "drive",
            Action::Fork => "fork",
            Action::Read => "read",
            Action::Write => "write",
            Action::Exec => "exec",
        }
    }

    /// Actions granted along with this one: drive includes suggest and view,
    /// suggest and fork include view, write includes read.
    const fn implied(self) -> &'static [Action] {
        match self {
            Action::Drive => &[Action::Suggest, Action::View],
            Action::Suggest | Action::Fork => &[Action::View],
            Action::Write => &[Action::Read],
            Action::View | Action::Read | Action::Exec => &[],
        }
    }
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A resource of the issuing node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resource {
    /// One session, by its `<session id>`.
    Session { id: String },
    /// One `<turn>` subtree of a session, by node id.
    Prompt { session: String, turn: NodeId },
    /// Every session whose `<session project>` is this project.
    Project { id: String },
    /// Every session the node creates at or after `after_ms`.
    FutureSessions { after_ms: i64 },
    /// One SSH host as named in the Remote hosts list.
    Ssh { host: String },
    /// One surface, by `surface://server/name` uri.
    Surface { uri: String },
}

impl Resource {
    pub(crate) const fn kind(&self) -> &'static str {
        match self {
            Resource::Session { .. } => "session",
            Resource::Prompt { .. } => "prompt",
            Resource::Project { .. } => "project",
            Resource::FutureSessions { .. } => "future sessions",
            Resource::Ssh { .. } => "ssh",
            Resource::Surface { .. } => "surface",
        }
    }

    fn supports(&self, action: Action) -> bool {
        use Action as A;
        match self {
            Resource::Session { .. }
            | Resource::Prompt { .. }
            | Resource::Project { .. }
            | Resource::FutureSessions { .. } => matches!(action, A::View | A::Suggest | A::Drive | A::Fork),
            Resource::Ssh { .. } => matches!(action, A::Read | A::Write | A::Exec),
            Resource::Surface { .. } => matches!(action, A::View | A::Drive),
        }
    }
}

/// A resource and the actions granted on it, implied actions included.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub resource: Resource,
    pub actions: BTreeSet<Action>,
}

impl Scope {
    /// Checks the actions fit the resource and adds the implied ones.
    pub fn new(resource: Resource, actions: impl IntoIterator<Item = Action>) -> Result<Self> {
        let mut set = BTreeSet::new();
        for action in actions {
            set.insert(action);
            set.extend(action.implied());
        }
        if set.is_empty() {
            return Err(GrantError::NoActions);
        }
        if let Some(&action) = set.iter().find(|a| !resource.supports(**a)) {
            return Err(GrantError::Action { resource: resource.kind(), action });
        }
        if let Resource::Prompt { turn, .. } = &resource {
            i64::try_from(turn.0).map_err(|_| GrantError::NodeId(*turn))?;
        }
        Ok(Self { resource, actions: set })
    }

    /// Revalidates scopes deserialized from storage or supplied as struct literals.
    pub(crate) fn validate(&self) -> Result<()> {
        if self.actions.is_empty() {
            return Err(GrantError::NoActions);
        }
        for &action in &self.actions {
            if !self.resource.supports(action) {
                return Err(GrantError::Action { resource: self.resource.kind(), action });
            }
        }
        if let Resource::Prompt { turn, .. } = &self.resource {
            i64::try_from(turn.0).map_err(|_| GrantError::NodeId(*turn))?;
        }
        Ok(())
    }
}

/// The facts about a session that grant checks need. The owner node reads
/// them from its own DOM, so a peer can't forge them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFacts {
    pub id: String,
    pub created_ms: i64,
    pub project: Option<String>,
}

impl SessionFacts {
    /// Reads the facts from the `<session>` root. `None` for an empty DOM.
    pub fn from_dom(dom: &Dom) -> Option<Self> {
        let root = dom.root()?;
        Some(Self {
            id: root.str_attr(attr::ID)?.to_owned(),
            created_ms: root.int_attr(attr::CREATED)?,
            project: root.str_attr(attr::PROJECT).map(str::to_owned),
        })
    }
}

/// The `<turn>` that `node` is in (or is), if any.
pub fn turn_of(dom: &Dom, node: NodeId) -> Option<NodeId> {
    let this = dom.get(node)?;
    if this.kind == ElementKind::Turn {
        return Some(node);
    }
    dom.ancestors(node).find(|n| n.kind == ElementKind::Turn).map(|n| n.id)
}

/// What a request acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    /// A session. `turn` names the turn the requested element belongs to, or
    /// `None` when the request is about the session as a whole.
    Session { session: &'a SessionFacts, turn: Option<NodeId> },
    Ssh { host: &'a str },
    Surface { uri: &'a str },
}

/// One thing a peer wants to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub action: Action,
    pub target: Target<'a>,
}

impl<'a> Request<'a> {
    /// `action` on a whole session.
    pub fn session(action: Action, session: &'a SessionFacts) -> Self {
        Self { action, target: Target::Session { session, turn: None } }
    }

    /// `action` on an element inside `turn` of a session.
    pub fn turn(action: Action, session: &'a SessionFacts, turn: NodeId) -> Self {
        Self { action, target: Target::Session { session, turn: Some(turn) } }
    }

    pub fn ssh(action: Action, host: &'a str) -> Self {
        Self { action, target: Target::Ssh { host } }
    }

    pub fn surface(action: Action, uri: &'a str) -> Self {
        Self { action, target: Target::Surface { uri } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_brings_suggest_and_view() {
        let scope = Scope::new(Resource::Session { id: "s".into() }, [Action::Drive]).unwrap();
        assert_eq!(scope.actions, BTreeSet::from([Action::View, Action::Suggest, Action::Drive]));
    }

    #[test]
    fn actions_must_fit_the_resource() {
        let ssh_view = Scope::new(Resource::Ssh { host: "box".into() }, [Action::View]);
        assert!(matches!(ssh_view, Err(GrantError::Action { action: Action::View, .. })));
        let surface_fork = Scope::new(Resource::Surface { uri: "surface://b/t".into() }, [Action::Fork]);
        assert!(matches!(surface_fork, Err(GrantError::Action { action: Action::Fork, .. })));
        assert!(matches!(Scope::new(Resource::Project { id: "p".into() }, []), Err(GrantError::NoActions)));
    }
}
