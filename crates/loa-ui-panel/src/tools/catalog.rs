//! What the page shows after grouping, filtering and toggles.

use super::model::{BuiltinGroup, BuiltinTools, McpServer, PeerTools, ToolId, ToolInfo};
use std::collections::BTreeSet;

/// Toggle state of a set of tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle { On, Off, Mixed }
impl Toggle {
    pub fn label(self) -> &'static str { ["on", "off", "mixed"][self as usize] }
}

/// A listed tool and whether it is on in the selected project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRow<'a> { pub id: ToolId, pub info: &'a ToolInfo, pub enabled: bool }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupView<'a> { pub group: BuiltinGroup, pub title: &'a str, pub tools: Vec<ToolRow<'a>> }
/// A server with its filtered tools; `toggle` is over all its tools, as the toggle sets all of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerView<'a> { pub server: &'a McpServer, pub toggle: Toggle, pub tools: Vec<ToolRow<'a>> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog<'a> { pub builtin: Vec<GroupView<'a>>, pub servers: Vec<ServerView<'a>> }
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

/// The Built in and MCP servers sections; groups and servers a filter leaves empty are dropped.
pub fn catalog<'a>(builtin: &'a BuiltinTools, servers: &'a [McpServer], disabled: &BTreeSet<ToolId>, filter: &Filter) -> Catalog<'a> {
    let row = |id: ToolId, info| ToolRow { enabled: !disabled.contains(&id), id, info };
    let groups = [(BuiltinGroup::Harness, builtin.harness.as_str(), &builtin.harness_tools), (BuiltinGroup::App, "App tools", &builtin.app_tools)];
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
pub struct PeerMatrix<'a> { pub peers: Vec<&'a PeerTools>, pub rows: Vec<MatrixRow> }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatrixRow { pub tool: ToolId, pub allowed: Vec<bool> }
/// The matrix over every listed tool (filtered as [`catalog`]); `only_peer` keeps that peer and the tools it may use.
pub fn peer_matrix<'a>(builtin: &BuiltinTools, servers: &[McpServer], peers: &'a [PeerTools], only_peer: Option<&str>, filter: &Filter) -> PeerMatrix<'a> {
    let peers: Vec<&PeerTools> = peers.iter().filter(|p| only_peer.is_none_or(|k| p.key == k)).collect();
    let builtin = builtin.harness_tools.iter().chain(&builtin.app_tools).filter(|t| filter.tool(t)).map(|t| ToolId::builtin(&t.name));
    let mcp = servers.iter().flat_map(|s| filter.server_tools(s).map(|t| ToolId::mcp(&s.name, &t.name)));
    let rows = builtin.chain(mcp).map(|tool| MatrixRow { allowed: peers.iter().map(|p| p.allows(&tool)).collect(), tool });
    PeerMatrix { rows: rows.filter(|r| only_peer.is_none() || r.allowed.iter().any(|a| *a)).collect(), peers }
}
