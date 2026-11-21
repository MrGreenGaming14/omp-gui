//! What the page shows after grouping, filtering and toggles.

use loa_view::panel::{BuiltinGroup, BuiltinTools, McpServer, PeerTools, ToolId, ToolInfo};
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
    pub fn matches(&self, text: &str) -> bool { self.is_empty() || self.matches_lower(&text.to_lowercase()) }
    /// [`Filter::matches`] for text that is already lowercase.
    fn matches_lower(&self, lower: &str) -> bool { lower.contains(&self.0) }
    fn tool(&self, t: &ToolInfo) -> bool { self.matches(&t.name) || self.matches(&t.description) }
    /// A matching server name keeps all its tools, else only matching tools stay.
    fn server_tools<'s>(&self, s: &'s McpServer) -> impl Iterator<Item = &'s ToolInfo> {
        let all = self.matches(&s.name);
        s.tools.iter().filter(move |t| all || self.tool(t))
    }
}

/// Lowercased names and descriptions of every tool, so filtering never lowercases again.
/// Entries pair with the builtin and server lists by index: rebuild it whenever those change.
#[derive(Clone, Debug, Default)]
pub struct Search { harness: Vec<Haystack>, app: Vec<Haystack>, servers: Vec<(String, Vec<Haystack>)> }

#[derive(Clone, Debug)]
struct Haystack { name: String, description: String }

impl Haystack {
    fn of(t: &ToolInfo) -> Self { Self { name: t.name.to_lowercase(), description: t.description.to_lowercase() } }
    fn matches(&self, filter: &Filter) -> bool { filter.matches_lower(&self.name) || filter.matches_lower(&self.description) }
}

fn haystacks(tools: &[ToolInfo]) -> Vec<Haystack> { tools.iter().map(Haystack::of).collect() }

impl Search {
    /// Lowercases every name and description once.
    pub fn new(builtin: &BuiltinTools, servers: &[McpServer]) -> Self {
        let servers = servers.iter().map(|s| (s.name.to_lowercase(), haystacks(&s.tools))).collect();
        Self { harness: haystacks(&builtin.harness_tools), app: haystacks(&builtin.app_tools), servers }
    }
}

/// A shown tool: its id, its index in the list it came from and whether it is on.
#[derive(Clone, Debug)]
struct Row { id: ToolId, index: usize, enabled: bool }

/// The Built in and MCP rows, filtered and toggled, held as indices into the lists their
/// [`Search`] was built from. Rebuild them whenever the filter, the selected project's disabled
/// tools or those lists change.
#[derive(Clone, Debug)]
pub struct Rows { builtin: Vec<(BuiltinGroup, Vec<Row>)>, servers: Vec<(usize, Toggle, Vec<Row>)> }

/// The rows of no tools: both groups, empty.
impl Default for Rows {
    fn default() -> Self { Self::new(&BuiltinTools::default(), &[], &Search::default(), &BTreeSet::new(), &Filter::default()) }
}

impl Rows {
    /// A server whose name matches the filter keeps all its tools, else only matching tools
    /// stay; groups and servers a filter leaves empty are dropped. `search` must be built from
    /// the same `builtin` and `servers`.
    pub fn new(builtin: &BuiltinTools, servers: &[McpServer], search: &Search, disabled: &BTreeSet<ToolId>, filter: &Filter) -> Self {
        let row = |id: ToolId, index| Row { enabled: !disabled.contains(&id), id, index };
        let rows = |tools: &[ToolInfo], found: &[Haystack]| -> Vec<Row> {
            tools.iter().zip(found).enumerate().filter(|(_, (_, h))| h.matches(filter)).map(|(i, (t, _))| row(ToolId::builtin(&t.name), i)).collect()
        };
        let groups = [(BuiltinGroup::Harness, rows(&builtin.harness_tools, &search.harness)), (BuiltinGroup::App, rows(&builtin.app_tools, &search.app))];
        let servers = servers.iter().zip(&search.servers).enumerate().filter_map(|(index, (s, (name, found)))| {
            let all = filter.matches_lower(name);
            let tools: Vec<Row> = s.tools.iter().zip(found).enumerate().filter(|(_, (_, h))| all || h.matches(filter)).map(|(i, (t, _))| row(ToolId::mcp(&s.name, &t.name), i)).collect();
            (all || !tools.is_empty()).then(|| (index, server_toggle(s, disabled), tools))
        });
        Self { builtin: groups.into_iter().filter(|(_, rows)| filter.is_empty() || !rows.is_empty()).collect(), servers: servers.collect() }
    }

    /// The sections as shown, borrowing names and infos from the lists the rows were built from.
    pub fn catalog<'a>(&self, builtin: &'a BuiltinTools, servers: &'a [McpServer]) -> Catalog<'a> {
        let groups = self.builtin.iter().map(|(group, rows)| {
            let (title, infos) = match group {
                BuiltinGroup::Harness => (builtin.harness.as_str(), &builtin.harness_tools),
                BuiltinGroup::App => (APP_GROUP_TITLE, &builtin.app_tools),
            };
            GroupView { group: *group, title, tools: tool_rows(rows, infos) }
        });
        let servers = self.servers.iter().filter_map(|(index, toggle, rows)| {
            let server = servers.get(*index)?;
            Some(ServerView { server, toggle: *toggle, tools: tool_rows(rows, &server.tools) })
        });
        Catalog { builtin: groups.collect(), servers: servers.collect() }
    }
}

/// Title of the app tools group.
pub const APP_GROUP_TITLE: &str = "App tools";

/// Cached rows against the infos they index; a stale index drops its row.
fn tool_rows<'a>(rows: &[Row], infos: &'a [ToolInfo]) -> Vec<ToolRow<'a>> {
    rows.iter().filter_map(|r| Some(ToolRow { id: r.id.clone(), info: infos.get(r.index)?, enabled: r.enabled })).collect()
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
/// The matrix over every listed tool (filtered as [`Rows`]); `only_peer` keeps that peer and the tools it may use.
pub fn peer_matrix<'a>(builtin: &BuiltinTools, servers: &[McpServer], peers: &'a [PeerTools], only_peer: Option<&str>, filter: &Filter) -> PeerMatrix<'a> {
    let peers: Vec<&PeerTools> = peers.iter().filter(|p| only_peer.is_none_or(|k| p.key == k)).collect();
    let builtin = builtin.harness_tools.iter().chain(&builtin.app_tools).filter(|t| filter.tool(t)).map(|t| ToolId::builtin(&t.name));
    let mcp = servers.iter().flat_map(|s| filter.server_tools(s).map(|t| ToolId::mcp(&s.name, &t.name)));
    let rows = builtin.chain(mcp).map(|tool| MatrixRow { allowed: peers.iter().map(|p| p.allows(&tool)).collect(), tool });
    PeerMatrix { rows: rows.filter(|r| only_peer.is_none() || r.allowed.iter().any(|a| *a)).collect(), peers }
}
