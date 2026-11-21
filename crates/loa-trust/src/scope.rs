//! What a grant covers and what a peer asks for. [`KINDS`] describes every
//! resource kind once; scope checks and the token Datalog are generated from it.

use crate::{Result, TrustError};
use biscuit_auth::builder::{Term, int, string};
use loa_core::{Dom, ElementKind, NodeId, attr};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

loa_util::str_enum! {
    /// One permission; [`ResourceKind::actions`] says which a kind takes.
    pub Action {
        /// Read the session or watch the surface.
        View => "view",
        /// Send prompts or edits the owner applies after review.
        Suggest => "suggest",
        /// Prompt the session, answer its decisions, use its tools; or send input to a surface.
        Drive => "drive",
        /// Copy the session and run it on the peer's own machine and keys.
        Fork => "fork",
        Read => "read",
        Write => "write",
        Exec => "exec",
    }
}

impl Action {
    /// Actions granted along with this one.
    const fn implied(self) -> &'static [Action] {
        match self {
            Action::Drive => &[Action::Suggest, Action::View],
            Action::Suggest | Action::Fork => &[Action::View],
            Action::Write => &[Action::Read],
            Action::View | Action::Read | Action::Exec => &[],
        }
    }
}

/// A resource of the issuing node. Its serde form is stored in grant definitions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resource {
    /// One session, by its `<session id>`.
    Session { id: String },
    /// One `<turn>` subtree of a session.
    Prompt { session: String, turn: NodeId },
    /// Every session whose `<session project>` is this project.
    Project { id: String },
    /// Every session the node creates at or after `after_ms`.
    FutureSessions { after_ms: i64 },
    /// One SSH host as named in the Remote hosts list.
    Ssh { host: String },
    /// One surface, by `surface://server/name` uri.
    Surface { uri: String },
    /// Running sandbox tasks on the issuing node.
    Sandbox,
    /// One folder and everything under it, by normalized absolute path.
    Folder { path: String },
}

/// One kind of [`Resource`]: its name, the actions it takes and its Datalog.
pub struct ResourceKind {
    pub name: &'static str,
    pub actions: &'static [Action],
    /// Predicate of the right fact in the authority block: `right(values.., action)`.
    pub right: &'static str,
    /// The request facts a right covers; `#i` stands for the right's i-th value.
    pub covers: &'static str,
    /// The resource from its right's values; `None` when they don't fit.
    build: fn(&[Term]) -> Option<Resource>,
}

const SESSION: &[Action] = &[Action::View, Action::Suggest, Action::Drive, Action::Fork];

/// The one value sandbox facts carry: the issuing node's sandbox.
pub(crate) const SANDBOX: &str = "node";

/// Every resource kind, in [`Resource`] variant order. The fact names are the
/// token format: existing grants verify only while they stay the same.
#[rustfmt::skip]
pub static KINDS: [ResourceKind; 8] = [
    ResourceKind { name: "session", actions: SESSION, right: "session_right", covers: "session(#0)",
        build: |v| Some(Resource::Session { id: text(v, 0)? }) },
    ResourceKind { name: "prompt", actions: SESSION, right: "prompt_right", covers: "session(#0), turn(#1)",
        build: |v| Some(Resource::Prompt { session: text(v, 0)?, turn: NodeId(u64::try_from(number(v, 1)?).ok()?) }) },
    ResourceKind { name: "project", actions: SESSION, right: "project_right", covers: "project(#0)",
        build: |v| Some(Resource::Project { id: text(v, 0)? }) },
    ResourceKind { name: "future sessions", actions: SESSION, right: "future_right", covers: "created($c), $c >= #0",
        build: |v| Some(Resource::FutureSessions { after_ms: number(v, 0)? }) },
    ResourceKind { name: "ssh", actions: &[Action::Read, Action::Write, Action::Exec], right: "ssh_right", covers: "ssh_host(#0)",
        build: |v| Some(Resource::Ssh { host: text(v, 0)? }) },
    ResourceKind { name: "surface", actions: &[Action::View, Action::Drive], right: "surface_right", covers: "surface(#0)",
        build: |v| Some(Resource::Surface { uri: text(v, 0)? }) },
    ResourceKind { name: "sandbox", actions: &[Action::Exec], right: "sandbox_right", covers: "sandbox(#0)",
        build: |_| Some(Resource::Sandbox) },
    ResourceKind { name: "folder", actions: &[Action::Read, Action::Write], right: "folder_right", covers: "folder(#0)",
        build: |v| Some(Resource::Folder { path: text(v, 0)? }) },
];

fn text(values: &[Term], i: usize) -> Option<String> { if let Some(Term::Str(s)) = values.get(i) { Some(s.clone()) } else { None } }

fn number(values: &[Term], i: usize) -> Option<i64> { if let Some(Term::Integer(n)) = values.get(i) { Some(*n) } else { None } }

impl ResourceKind {
    /// How many values its right fact carries before the action.
    pub(crate) fn arity(&self) -> usize { self.covers.matches('#').count() }
    /// `$v0, $v1, ` for the right's values.
    pub(crate) fn vars(&self) -> String { (0..self.arity()).map(|i| format!("$v{i}, ")).collect() }
    /// [`ResourceKind::covers`] with `#i` replaced by `value(i)`.
    pub(crate) fn covers_with(&self, value: impl Fn(usize) -> String) -> String {
        (0..self.arity()).fold(self.covers.to_owned(), |body, i| body.replace(&format!("#{i}"), &value(i)))
    }

    pub(crate) fn build(&self, values: &[Term]) -> Option<Resource> { (self.build)(values) }
}

impl Resource {
    /// Its kind and the values of its right fact.
    pub(crate) fn parts(&self) -> (&'static ResourceKind, Vec<Term>) {
        match self {
            Resource::Session { id } => (&KINDS[0], vec![string(id)]),
            // `validate` checked the id fits an i64.
            Resource::Prompt { session, turn } => (&KINDS[1], vec![string(session), int(turn.0 as i64)]),
            Resource::Project { id } => (&KINDS[2], vec![string(id)]),
            Resource::FutureSessions { after_ms } => (&KINDS[3], vec![int(*after_ms)]),
            Resource::Ssh { host } => (&KINDS[4], vec![string(host)]),
            Resource::Surface { uri } => (&KINDS[5], vec![string(uri)]),
            Resource::Sandbox => (&KINDS[6], vec![string(SANDBOX)]),
            Resource::Folder { path } => (&KINDS[7], vec![string(path)]),
        }
    }

    pub fn kind(&self) -> &'static ResourceKind { self.parts().0 }
}

/// A resource and the actions granted on it, implied actions included.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub resource: Resource,
    pub actions: BTreeSet<Action>,
}

impl Scope {
    /// Checks the actions fit the resource and adds the implied ones it takes.
    pub fn new(resource: Resource, actions: impl IntoIterator<Item = Action>) -> Result<Self> {
        let takes = resource.kind().actions;
        let mut set = BTreeSet::new();
        for action in actions {
            set.insert(action);
            set.extend(action.implied().iter().filter(|implied| takes.contains(implied)));
        }
        let scope = Self { resource, actions: set };
        scope.validate()?;
        Ok(scope)
    }

    /// Checks actions, prompt ids (fit an i64) and folder paths (absolute, normalized).
    pub(crate) fn validate(&self) -> Result<()> {
        let kind = self.resource.kind();
        if self.actions.is_empty() {
            return Err(TrustError::NoActions);
        }
        if let Some(&action) = self.actions.iter().find(|a| !kind.actions.contains(a)) {
            return Err(TrustError::Action { resource: kind.name, action });
        }
        match &self.resource {
            Resource::Prompt { turn, .. } if i64::try_from(turn.0).is_err() => Err(TrustError::NodeId(*turn)),
            Resource::Folder { path } if !folder_ancestors(path).is_some_and(|a| a.last() == Some(path)) => Err(TrustError::FolderPath(path.clone())),
            _ => Ok(()),
        }
    }
}

/// Every folder from the root down to `path` (`/`, `/a`, `/a/b`); `None` when
/// `path` isn't absolute or has `.`/`..`.
pub(crate) fn folder_ancestors(path: &str) -> Option<Vec<String>> {
    use std::path::{Component, Path, PathBuf};
    let path = Path::new(path);
    let mut out = Vec::new();
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            // A Windows prefix comes before its root: the first folder is both.
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir | Component::Normal(_) => {
                current.push(component.as_os_str());
                out.push(current.to_string_lossy().into_owned());
            }
            Component::CurDir | Component::ParentDir => return None,
        }
    }
    path.is_absolute().then_some(out)
}

/// The facts about a session grant checks need, read by the owner from its
/// own DOM so a peer can't forge them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFacts {
    pub id: String,
    pub created_ms: i64,
    pub project: Option<String>,
}

impl SessionFacts {
    /// Reads the facts from the `<session>` root; `None` for an empty DOM.
    pub fn from_dom(dom: &Dom) -> Option<Self> {
        let root = dom.root()?;
        let project = root.str_attr(attr::PROJECT).map(str::to_owned);
        Some(Self { id: root.str_attr(attr::ID)?.to_owned(), created_ms: root.int_attr(attr::CREATED)?, project })
    }
}

/// The `<turn>` that `node` is in (or is), if any.
pub fn turn_of(dom: &Dom, node: NodeId) -> Option<NodeId> {
    if dom.get(node)?.kind == ElementKind::Turn {
        return Some(node);
    }
    dom.ancestors(node).find(|n| n.kind == ElementKind::Turn).map(|n| n.id)
}

/// What a request acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    /// A session; `turn` is the turn the requested element is in, `None` for the whole session.
    Session { session: &'a SessionFacts, turn: Option<NodeId> },
    Ssh { host: &'a str },
    Surface { uri: &'a str },
    Sandbox,
    /// A folder by normalized absolute path: grants on it or above it cover it.
    Folder { path: &'a str },
}

/// One thing a peer wants to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub action: Action,
    pub target: Target<'a>,
}

impl<'a> Request<'a> {
    pub fn session(action: Action, session: &'a SessionFacts) -> Self { Self { action, target: Target::Session { session, turn: None } } }
    /// `action` on an element inside `turn` of a session.
    pub fn turn(action: Action, session: &'a SessionFacts, turn: NodeId) -> Self {
        Self { action, target: Target::Session { session, turn: Some(turn) } }
    }
    pub fn ssh(action: Action, host: &'a str) -> Self { Self { action, target: Target::Ssh { host } } }
    pub fn surface(action: Action, uri: &'a str) -> Self { Self { action, target: Target::Surface { uri } } }
    pub fn sandbox(action: Action) -> Self { Self { action, target: Target::Sandbox } }
    pub fn folder(action: Action, path: &'a str) -> Self { Self { action, target: Target::Folder { path } } }
}
