//! What the page shows after grouping, filtering and toggles.

use super::model::{BuiltinGroup, BuiltinTools, McpServer, PeerTools, ToolId, ToolInfo};
use std::collections::BTreeSet;

/// Toggle state of a set of tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    On,
    Off,
    Mixed,
}

impl Toggle {
    pub fn label(self) -> &'static str { ["on", "off", "mixed"][self as usize] }
    /// Clicking turns anything not fully on, on.
    pub fn next_enabled(self) -> bool { self != Self::On }
}

/// A listed tool and whether it is on in the selected project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRow<'a> {
    pub id: ToolId,
    pub info: &'a ToolInfo,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupView<'a> {
    pub group: BuiltinGroup,
    pub title: &'a str,
    pub tools: Vec<ToolRow<'a>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerView<'a> {
    pub server: &'a McpServer,
    /// Over all the server's tools, not just the filtered ones: the toggle sets all of them.
    pub toggle: Toggle,
    pub tools: Vec<ToolRow<'a>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog<'a> {
    pub builtin: Vec<GroupView<'a>>,
    pub servers: Vec<ServerView<'a>>,
}

/// A lowercase text filter; empty matches everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter(String);

impl Filter {
    pub fn new(text: &str) -> Self { Self(text.trim().to_lowercase()) }
    pub fn is_empty(&self) -> bool { self.0.is_empty() }
    pub fn matches(&self, text: &str) -> bool { self.0.is_empty() || text.to_lowercase().contains(&self.0) }
    fn tool(&self, t: &ToolInfo) -> bool { self.matches(&t.name) || self.matches(&t.description) }
    /// A matching server name keeps all its tools, else only matching tools stay.
    fn server_tools<'s>(&self, s: &'s McpServer) -> impl Iterator<Item = &'s ToolInfo> {
        let all = self.matches(&s.name);
        s.tools.iter().filter(move |t| all || self.tool(t))
    }
}

pub const APP_GROUP_TITLE: &str = "App tools";

/// The Built in and MCP servers sections; groups and servers a filter leaves empty are dropped.
pub fn catalog<'a>(builtin: &'a BuiltinTools, servers: &'a [McpServer], disabled: &BTreeSet<ToolId>, filter: &Filter) -> Catalog<'a> {
    let row = |id: ToolId, info| ToolRow { enabled: !disabled.contains(&id), id, info };
    let groups = [(BuiltinGroup::Harness, builtin.harness.as_str(), &builtin.harness_tools), (BuiltinGroup::App, APP_GROUP_TITLE, &builtin.app_tools)];
    let builtin = groups.into_iter().map(|(group, title, tools)| GroupView { group, title, tools: tools.iter().filter(|t| filter.tool(t)).map(|t| row(ToolId::builtin(&t.name), t)).collect() });
    let servers = servers.iter().filter_map(|s| {
        let tools: Vec<_> = filter.server_tools(s).map(|t| row(ToolId::mcp(&s.name, &t.name), t)).collect();
        (filter.matches(&s.name) || !tools.is_empty()).then(|| ServerView { server: s, toggle: server_toggle(s, disabled), tools })
    });
    Catalog { builtin: builtin.filter(|g| filter.is_empty() || !g.tools.is_empty()).collect(), servers: servers.collect() }
}

/// On, off or mixed over every tool of a server; a server without tools is on.
pub fn server_toggle(s: &McpServer, disabled: &BTreeSet<ToolId>) -> Toggle {
    match s.tools.iter().filter(|t| disabled.contains(&ToolId::mcp(&s.name, &t.name))).count() {
        0 => Toggle::On,
        n if n == s.tools.len() => Toggle::Off,
        _ => Toggle::Mixed,
    }
}

/// Which tools each peer may use: `rows[i].allowed[j]` for `peers[j]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerMatrix<'a> {
    pub peers: Vec<&'a PeerTools>,
    pub rows: Vec<MatrixRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatrixRow {
    pub tool: ToolId,
    pub allowed: Vec<bool>,
}

/// The matrix over every listed tool; with `only_peer` it has that peer and the tools it may
/// use. The filter applies as in [`catalog`].
pub fn peer_matrix<'a>(builtin: &BuiltinTools, servers: &[McpServer], peers: &'a [PeerTools], only_peer: Option<&str>, filter: &Filter) -> PeerMatrix<'a> {
    let peers: Vec<&PeerTools> = peers.iter().filter(|p| only_peer.is_none_or(|k| p.key == k)).collect();
    let builtin = builtin.harness_tools.iter().chain(&builtin.app_tools).filter(|t| filter.tool(t)).map(|t| ToolId::builtin(&t.name));
    let mcp = servers.iter().flat_map(|s| filter.server_tools(s).map(|t| ToolId::mcp(&s.name, &t.name)));
    let rows = builtin.chain(mcp).map(|tool| MatrixRow { allowed: peers.iter().map(|p| p.allows(&tool)).collect(), tool });
    PeerMatrix { rows: rows.filter(|r| only_peer.is_none() || r.allowed.iter().any(|a| *a)).collect(), peers }
}

#[cfg(test)]
mod tests {
    use super::super::model::{ServerKind, ServerStatus, ToolRule};
    use super::*;
    use loa_contracts::harness::McpTransport;

    fn tools(list: &[(&str, &str)]) -> Vec<ToolInfo> { list.iter().map(|(n, d)| ToolInfo::new(*n, *d)).collect() }

    fn fixture() -> (BuiltinTools, Vec<McpServer>) {
        let builtin = BuiltinTools { harness: "omp".into(), harness_tools: tools(&[("bash", "Run a shell command"), ("read", "Read a file")]), app_tools: tools(&[("project_search", "Search sessions"), ("project_read", "Read a session"), ("sandbox_task", "Run a task in a sandbox")]), harness_note: None };
        let server = |name: &str, list| McpServer { name: name.into(), transport: McpTransport::Http { url: format!("http://localhost/{name}"), headers: Default::default() }, status: ServerStatus::Running, kind: ServerKind::External, tools: tools(list) };
        // `files` has a tool named like a built in one: it stays under its server.
        (builtin, vec![server("remote", &[("remote_bash", "Run a command on an SSH host"), ("remote_file", "Files on an SSH host")]), server("browser", &[("surface_screenshot", "Capture"), ("surface_click", "Click")]), server("files", &[("read", "Read via MCP")])])
    }

    fn names<'a>(c: &Catalog<'a>) -> (Vec<Vec<&'a str>>, Vec<Vec<&'a str>>) {
        let n = |rows: &[ToolRow<'a>]| rows.iter().map(|r| r.info.name.as_str()).collect::<Vec<_>>();
        (c.builtin.iter().map(|g| n(&g.tools)).collect(), c.servers.iter().map(|s| n(&s.tools)).collect())
    }

    #[test]
    fn catalog_groups_filters_and_scopes_toggles() {
        let (builtin, servers) = fixture();
        let none = BTreeSet::new();
        let cases = [
            ("", vec![vec!["bash", "read"], vec!["project_search", "project_read", "sandbox_task"]], vec![vec!["remote_bash", "remote_file"], vec!["surface_screenshot", "surface_click"], vec!["read"]]),
            ("PROJECT_", vec![vec!["project_search", "project_read"]], vec![]),
            ("ssh host", vec![], vec![vec!["remote_bash", "remote_file"]]),
            ("brow", vec![], vec![vec!["surface_screenshot", "surface_click"]]),
            ("click", vec![], vec![vec!["surface_click"]]),
        ];
        for (f, b, s) in cases { assert_eq!(names(&catalog(&builtin, &servers, &none, &Filter::new(f))), (b, s), "{f:?}") }
        let disabled = BTreeSet::from([ToolId::builtin("read"), ToolId::mcp("browser", "surface_screenshot")]);
        let c = catalog(&builtin, &servers, &disabled, &Filter::new("read"));
        assert_eq!((c.builtin[0].tools[0].enabled, c.servers[0].tools[0].enabled), (false, true), "a built in toggle leaves the MCP tool alone");
        assert_eq!(catalog(&builtin, &servers, &disabled, &Filter::new("click")).servers[0].toggle, Toggle::Mixed, "the toggle counts filtered out tools");
        let toggles = [(vec![], Toggle::On), (vec![ToolId::mcp("remote", "remote_bash")], Toggle::Mixed), (vec![ToolId::mcp("remote", "remote_bash"), ToolId::mcp("remote", "remote_file")], Toggle::Off), (vec![ToolId::mcp("browser", "remote_bash")], Toggle::On)];
        for (off, want) in toggles { assert_eq!(server_toggle(&servers[0], &off.into_iter().collect()), want) }
    }

    #[test]
    fn peer_matrix_follows_grant_rules() {
        let (builtin, servers) = fixture();
        let peer = |k: &str, rules| PeerTools { key: k.into(), name: k.into(), rules };
        let peers = vec![peer("alice", vec![ToolRule::All]), peer("bob", vec![ToolRule::Tool(ToolId::builtin("read")), ToolRule::Server("browser".into())]), peer("carol", vec![ToolRule::AllBuiltin])];
        let m = peer_matrix(&builtin, &servers, &peers, None, &Filter::default());
        let row = |id: ToolId| m.rows.iter().find(|r| r.tool == id).unwrap().allowed.clone();
        assert_eq!(m.rows.len(), 10);
        let cases = [(ToolId::builtin("bash"), [true, false, true]), (ToolId::builtin("read"), [true, true, true]), (ToolId::mcp("files", "read"), [true, false, false]), (ToolId::mcp("browser", "surface_click"), [true, true, false])];
        for (id, want) in cases { assert_eq!(row(id), want) }
        let bob = peer_matrix(&builtin, &servers, &peers, Some("bob"), &Filter::default());
        assert_eq!(bob.rows.iter().map(|r| r.tool.name()).collect::<Vec<_>>(), ["read", "surface_screenshot", "surface_click"]);
        assert_eq!(peer_matrix(&builtin, &servers, &peers, Some("bob"), &Filter::new("click")).rows.len(), 1);
    }
}
