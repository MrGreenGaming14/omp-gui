//! What a grant covers and what a peer asks for.

use crate::{GrantError, Result};
use loa_core::{Dom, ElementKind, NodeId, attr};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One permission. Sessions (and prompts, projects, future sessions) take
/// view, suggest, drive and fork; surfaces take view and drive; SSH hosts take
/// read, write and exec; folders take read and write.
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
    /// SSH or folder: read files.
    Read,
    /// SSH or folder: write files.
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

    /// The action named `text` ([`Action::as_str`]).
    pub fn parse(text: &str) -> Option<Self> {
        [Action::View, Action::Suggest, Action::Drive, Action::Fork, Action::Read, Action::Write, Action::Exec]
            .into_iter()
            .find(|a| a.as_str() == text)
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
    /// Running sandbox tasks on the issuing node (its sandbox providers).
    Sandbox,
    /// One folder of the issuing node and everything under it, by absolute
    /// path. Read lists and reads its files; write also writes them.
    Folder { path: String },
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
            Resource::Sandbox => "sandbox",
            Resource::Folder { .. } => "folder",
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
            Resource::Sandbox => matches!(action, A::Exec),
            Resource::Folder { .. } => matches!(action, A::Read | A::Write),
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
    /// Checks the actions fit the resource and adds the implied ones the
    /// resource takes (a surface's drive brings view, not suggest).
    pub fn new(resource: Resource, actions: impl IntoIterator<Item = Action>) -> Result<Self> {
        let mut set = BTreeSet::new();
        for action in actions {
            set.insert(action);
            set.extend(action.implied().iter().filter(|implied| resource.supports(**implied)));
        }
        if set.is_empty() {
            return Err(GrantError::NoActions);
        }
        if let Some(&action) = set.iter().find(|a| !resource.supports(**a)) {
            return Err(GrantError::Action { resource: resource.kind(), action });
        }
        resource.validate()?;
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
        self.resource.validate()
    }
}

impl Resource {
    /// Checks what the resource names fits a grant: prompt ids fit an i64,
    /// folder paths are absolute and normalized (no `.`, `..` or trailing `/`).
    fn validate(&self) -> Result<()> {
        match self {
            Resource::Prompt { turn, .. } => {
                i64::try_from(turn.0).map_err(|_| GrantError::NodeId(*turn))?;
            }
            Resource::Folder { path } => {
                let normal = folder_ancestors(path).is_some_and(|ancestors| ancestors.last() == Some(path));
                if !normal {
                    return Err(GrantError::FolderPath(path.clone()));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Every folder from the root down to `path`, as normalized absolute paths
/// (`/`, `/a`, `/a/b`). `None` when `path` isn't absolute or has `.`/`..`.
pub(crate) fn folder_ancestors(path: &str) -> Option<Vec<String>> {
    use std::path::{Component, Path, PathBuf};
    let path = Path::new(path);
    if !path.is_absolute() {
        return None;
    }
    let mut out = Vec::new();
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            // A Windows prefix comes before its root: the first folder is both.
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => {
                current.push(component.as_os_str());
                out.push(current.to_string_lossy().into_owned());
            }
            Component::Normal(name) => {
                current.push(name);
                out.push(current.to_string_lossy().into_owned());
            }
            Component::CurDir | Component::ParentDir => return None,
        }
    }
    Some(out)
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
    /// The issuing node's sandbox.
    Sandbox,
    /// A folder of the issuing node, by normalized absolute path: grants on
    /// it or any folder above it cover the request.
    Folder { path: &'a str },
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

    /// `action` (`Exec`) on this node's sandbox.
    pub fn sandbox(action: Action) -> Self {
        Self { action, target: Target::Sandbox }
    }

    /// `action` (`Read` or `Write`) on the folder at `path` of this node.
    pub fn folder(action: Action, path: &'a str) -> Self {
        Self { action, target: Target::Folder { path } }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_brings_suggest_and_view() {
        let scope = Scope::new(Resource::Session { id: "s".into() }, [Action::Drive]).unwrap();
        assert_eq!(scope.actions, BTreeSet::from([Action::View, Action::Suggest, Action::Drive]));
        // A surface takes no suggest: its drive brings only view.
        let surface = Scope::new(Resource::Surface { uri: "surface://b/t".into() }, [Action::Drive]).unwrap();
        assert_eq!(surface.actions, BTreeSet::from([Action::View, Action::Drive]));
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
