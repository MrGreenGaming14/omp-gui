//! What a verified grant lets a peer see and use: subtrees of a session DOM,
//! sessions and digests for project context, and tools.

use crate::scope::{Action, Request, SessionFacts, turn_of};
use crate::token::VerifiedGrant;
use loa_core::{Dom, ElementKind, NodeId};
use loa_surface::SurfaceUri;

/// The parts of one session DOM a grant opens for an action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Visibility {
    Nothing,
    /// The whole session.
    Whole,
    /// Only these `<turn>` subtrees, in document order. The `<session>`
    /// element itself and everything outside the turns stays hidden.
    Turns(Vec<NodeId>),
}

impl Visibility {
    /// True when `node` lies in a visible part of `dom`.
    pub fn allows(&self, dom: &Dom, node: NodeId) -> bool {
        match self {
            Visibility::Nothing => false,
            Visibility::Whole => dom.contains(node),
            Visibility::Turns(turns) => turn_of(dom, node).is_some_and(|t| turns.contains(&t)),
        }
    }
}

impl VerifiedGrant {
    /// Which subtrees of `dom` the holder may `action` (view, suggest, drive,
    /// fork) at `now_ms`: the whole session or single turns.
    pub fn visibility(&self, dom: &Dom, action: Action, now_ms: i64) -> Visibility {
        let Some(facts) = SessionFacts::from_dom(dom) else { return Visibility::Nothing };
        if self.authorize(&Request::session(action, &facts), now_ms).is_ok() {
            return Visibility::Whole;
        }
        let Some(root) = dom.root() else { return Visibility::Nothing };
        let turns: Vec<NodeId> = dom
            .children_of_kind(root.id, ElementKind::Turn)
            .filter(|t| self.authorize(&Request::turn(action, &facts, t.id), now_ms).is_ok())
            .map(|t| t.id)
            .collect();
        if turns.is_empty() { Visibility::Nothing } else { Visibility::Turns(turns) }
    }

    /// The sessions whose whole DOM, and so whose `<digest>`, the holder may
    /// view. Project context, `project_search` and `project_read` filter
    /// through this, so a future sessions grant never sees earlier sessions
    /// or their digests.
    pub fn visible_sessions<'s>(
        &self,
        sessions: impl IntoIterator<Item = &'s SessionFacts>,
        now_ms: i64,
    ) -> Vec<&'s SessionFacts> {
        sessions.into_iter().filter(|s| self.authorize(&Request::session(Action::View, s), now_ms).is_ok()).collect()
    }
}

/// Name of the app's MCP server for SSH tools.
pub const REMOTE_SERVER: &str = "remote";

/// Prefix of the tools surface MCP servers expose.
const SURFACE_TOOL_PREFIX: &str = "surface_";

/// Where a tool comes from, as the tools panel lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolSource {
    /// Harness tools and the app's own tools.
    Builtin,
    Mcp { server: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tool {
    pub source: ToolSource,
    pub name: String,
}

/// Which tools a peer may have the agent use in one session through its
/// grant. The tools panel and the MCP client call [`ToolAccess::allows`].
///
/// - Every tool needs drive on the session.
/// - Tools of the `remote` server also need an SSH grant on at least one of
///   the configured hosts: `remote_bash` needs exec, other tools read.
///   Each call still checks the host it targets with
///   [`VerifiedGrant::authorize`] and [`Request::ssh`].
/// - `surface_*` tools of a server also need a surface grant on one of that
///   server's open surfaces: view for `surface_screenshot`, drive otherwise.
#[derive(Debug)]
pub struct ToolAccess<'a> {
    grant: &'a VerifiedGrant,
    session: &'a SessionFacts,
    ssh_hosts: &'a [String],
    surfaces: &'a [SurfaceUri],
    now_ms: i64,
}

impl<'a> ToolAccess<'a> {
    /// `ssh_hosts` are the Remote hosts this node knows, `surfaces` the
    /// surfaces open in the session.
    pub fn new(
        grant: &'a VerifiedGrant,
        session: &'a SessionFacts,
        ssh_hosts: &'a [String],
        surfaces: &'a [SurfaceUri],
        now_ms: i64,
    ) -> Self {
        Self { grant, session, ssh_hosts, surfaces, now_ms }
    }

    fn may(&self, request: &Request<'_>) -> bool {
        self.grant.authorize(request, self.now_ms).is_ok()
    }
    /// Checks the actual host and operation of an SSH tool call. The tool
    /// listing is only a preview; a writable `remote_file` call must not
    /// inherit the read permission used to display that tool.
    pub fn allows_ssh_call(&self, host: &str, action: Action) -> bool {
        matches!(action, Action::Read | Action::Write | Action::Exec)
            && self.may(&Request::session(Action::Drive, self.session))
            && self.may(&Request::ssh(action, host))
    }

    pub fn allows(&self, tool: &Tool) -> bool {
        if !self.may(&Request::session(Action::Drive, self.session)) {
            return false;
        }
        match &tool.source {
            ToolSource::Builtin => true,
            ToolSource::Mcp { server } if server == REMOTE_SERVER => {
                let action = if tool.name == "remote_bash" { Action::Exec } else { Action::Read };
                self.ssh_hosts.iter().any(|host| self.may(&Request::ssh(action, host)))
            }
            ToolSource::Mcp { server } if tool.name.starts_with(SURFACE_TOOL_PREFIX) => {
                let action = if tool.name == "surface_screenshot" { Action::View } else { Action::Drive };
                self.surfaces
                    .iter()
                    .filter(|uri| uri.server() == server)
                    .any(|uri| self.may(&Request::surface(action, uri.as_str())))
            }
            ToolSource::Mcp { .. } => true,
        }
    }

    /// The tools of `tools` the peer may use, in order.
    pub fn filter<'t>(&self, tools: &'t [Tool]) -> Vec<&'t Tool> {
        tools.iter().filter(|t| self.allows(t)).collect()
    }
}
