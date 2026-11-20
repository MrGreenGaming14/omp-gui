//! View model: what the panel shows after grouping, filtering and toggles.

use std::collections::BTreeSet;

use crate::model::{BuiltinGroup, BuiltinTools, McpServer, PeerTools, ToolId, ToolInfo};

/// Toggle state of a tool set as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    On,
    Off,
    /// Some tools on, some off.
    Mixed,
}

impl Toggle {
    pub fn label(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::Mixed => "mixed",
        }
    }

    /// What clicking the toggle turns every tool into: anything not fully on turns on.
    pub fn next_enabled(self) -> bool {
        self != Self::On
    }
}

/// One listed tool with its state in the current project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRow<'a> {
    pub id: ToolId,
    pub info: &'a ToolInfo,
    pub enabled: bool,
}

/// One group of the Built in section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupView<'a> {
    pub group: BuiltinGroup,
    pub title: &'a str,
    pub tools: Vec<ToolRow<'a>>,
}

/// One server of the MCP servers section with its own tools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerView<'a> {
    pub server: &'a McpServer,
    /// State over all of the server's tools, not just the filtered ones,
    /// because the server toggle sets all of them.
    pub toggle: Toggle,
    pub tools: Vec<ToolRow<'a>>,
}

/// Both tool sections, filtered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog<'a> {
    pub builtin: Vec<GroupView<'a>>,
    pub servers: Vec<ServerView<'a>>,
}

/// Lowercased text filter. Empty matches everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter(String);

impl Filter {
    pub fn new(text: &str) -> Self {
        Self(text.trim().to_lowercase())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn matches(&self, text: &str) -> bool {
        self.is_empty() || self.matches_lower(&text.to_lowercase())
    }

    /// [`Filter::matches`] for text that is already lowercase.
    fn matches_lower(&self, lower: &str) -> bool {
        lower.contains(&self.0)
    }

    fn matches_tool(&self, tool: &ToolInfo) -> bool {
        self.matches(&tool.name) || self.matches(&tool.description)
    }
}

/// Title of the app tools group.
pub const APP_GROUP_TITLE: &str = "App tools";

/// Lowercased names and descriptions of every tool, so filtering never
/// lowercases again. Entries pair with the builtin and server lists by index,
/// so rebuild the search whenever those lists change.
#[derive(Clone, Debug, Default)]
pub struct Search {
    harness: Vec<Haystack>,
    app: Vec<Haystack>,
    servers: Vec<ServerSearch>,
}

#[derive(Clone, Debug)]
struct ServerSearch {
    name: String,
    tools: Vec<Haystack>,
}

#[derive(Clone, Debug)]
struct Haystack {
    name: String,
    description: String,
}

impl Haystack {
    fn of(tool: &ToolInfo) -> Self {
        Self {
            name: tool.name.to_lowercase(),
            description: tool.description.to_lowercase(),
        }
    }

    fn matches(&self, filter: &Filter) -> bool {
        filter.matches_lower(&self.name) || filter.matches_lower(&self.description)
    }
}

fn haystacks(tools: &[ToolInfo]) -> Vec<Haystack> {
    tools.iter().map(Haystack::of).collect()
}

impl Search {
    /// Lowercases every name and description once.
    pub fn new(builtin: &BuiltinTools, servers: &[McpServer]) -> Self {
        Self {
            harness: haystacks(&builtin.harness_tools),
            app: haystacks(&builtin.app_tools),
            servers: servers
                .iter()
                .map(|server| ServerSearch {
                    name: server.name.to_lowercase(),
                    tools: haystacks(&server.tools),
                })
                .collect(),
        }
    }
}

/// The Built in and MCP rows, filtered and toggled, held as indices into the
/// lists their [`Search`] was built from. Rebuild them whenever the filter, the
/// selected project's disabled tools or those lists change.
#[derive(Clone, Debug)]
pub struct Rows {
    builtin: Vec<GroupRows>,
    servers: Vec<ServerRows>,
}

#[derive(Clone, Debug)]
struct GroupRows {
    group: BuiltinGroup,
    rows: Vec<Row>,
}

#[derive(Clone, Debug)]
struct ServerRows {
    server: usize,
    toggle: Toggle,
    rows: Vec<Row>,
}

#[derive(Clone, Debug)]
struct Row {
    id: ToolId,
    index: usize,
    enabled: bool,
}

impl Default for Rows {
    fn default() -> Self {
        Self::new(
            &BuiltinTools::default(),
            &[],
            &Search::default(),
            &BTreeSet::new(),
            &Filter::default(),
        )
    }
}

impl Rows {
    /// Builds the Built in and MCP servers rows. A server whose name matches
    /// the filter keeps all its tools; otherwise only matching tools are kept.
    /// Groups and servers left empty by a non-empty filter are dropped.
    /// `search` must be built from the same `builtin` and `servers`.
    pub fn new(
        builtin: &BuiltinTools,
        servers: &[McpServer],
        search: &Search,
        disabled: &BTreeSet<ToolId>,
        filter: &Filter,
    ) -> Self {
        let builtin_rows = |tools: &[ToolInfo], lowered: &[Haystack]| -> Vec<Row> {
            tools
                .iter()
                .zip(lowered)
                .enumerate()
                .filter(|(_, (_, haystack))| haystack.matches(filter))
                .map(|(index, (tool, _))| {
                    let id = ToolId::builtin(&tool.name);
                    let enabled = !disabled.contains(&id);
                    Row { id, index, enabled }
                })
                .collect()
        };
        let groups = [
            GroupRows {
                group: BuiltinGroup::Harness,
                rows: builtin_rows(&builtin.harness_tools, &search.harness),
            },
            GroupRows {
                group: BuiltinGroup::App,
                rows: builtin_rows(&builtin.app_tools, &search.app),
            },
        ];
        let builtin_groups: Vec<GroupRows> = groups
            .into_iter()
            .filter(|group| filter.is_empty() || !group.rows.is_empty())
            .collect();
        let server_rows: Vec<ServerRows> = servers
            .iter()
            .zip(&search.servers)
            .enumerate()
            .filter_map(|(server_index, (server, searched))| {
                let name_matches = filter.matches_lower(&searched.name);
                let rows: Vec<Row> = server
                    .tools
                    .iter()
                    .zip(&searched.tools)
                    .enumerate()
                    .filter(|(_, (_, haystack))| name_matches || haystack.matches(filter))
                    .map(|(index, (tool, _))| {
                        let id = ToolId::mcp(&server.name, &tool.name);
                        let enabled = !disabled.contains(&id);
                        Row { id, index, enabled }
                    })
                    .collect();
                if !name_matches && rows.is_empty() {
                    return None;
                }
                Some(ServerRows {
                    server: server_index,
                    toggle: server_toggle(server, disabled),
                    rows,
                })
            })
            .collect();
        Self {
            builtin: builtin_groups,
            servers: server_rows,
        }
    }

    /// The sections as shown, borrowing names and infos from the lists the rows
    /// were built from.
    pub fn catalog<'a>(&self, builtin: &'a BuiltinTools, servers: &'a [McpServer]) -> Catalog<'a> {
        let groups: Vec<GroupView<'a>> = self
            .builtin
            .iter()
            .map(|group| {
                let (title, infos) = match group.group {
                    BuiltinGroup::Harness => (builtin.harness.as_str(), &builtin.harness_tools),
                    BuiltinGroup::App => (APP_GROUP_TITLE, &builtin.app_tools),
                };
                GroupView {
                    group: group.group,
                    title,
                    tools: tool_rows(&group.rows, infos),
                }
            })
            .collect();
        let server_views: Vec<ServerView<'a>> = self
            .servers
            .iter()
            .filter_map(|group| {
                let server = servers.get(group.server)?;
                Some(ServerView {
                    server,
                    toggle: group.toggle,
                    tools: tool_rows(&group.rows, &server.tools),
                })
            })
            .collect();
        Catalog {
            builtin: groups,
            servers: server_views,
        }
    }
}

/// Materializes cached rows against the infos they index. A stale index drops
/// its row instead of panicking.
fn tool_rows<'a>(rows: &[Row], infos: &'a [ToolInfo]) -> Vec<ToolRow<'a>> {
    rows.iter()
        .filter_map(|row| {
            Some(ToolRow {
                id: row.id.clone(),
                info: infos.get(row.index)?,
                enabled: row.enabled,
            })
        })
        .collect()
}

/// Toggle state over every tool of a server. A server with no tools is on.
pub fn server_toggle(server: &McpServer, disabled: &BTreeSet<ToolId>) -> Toggle {
    let off = server
        .tools
        .iter()
        .filter(|tool| disabled.contains(&ToolId::mcp(&server.name, &tool.name)))
        .count();
    match off {
        0 => Toggle::On,
        n if n == server.tools.len() => Toggle::Off,
        _ => Toggle::Mixed,
    }
}

/// Which tools each peer may use. `allowed[i]` belongs to `peers[i]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerMatrix<'a> {
    pub peers: Vec<&'a PeerTools>,
    pub rows: Vec<MatrixRow>,
}

/// One tool's line in the [`PeerMatrix`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatrixRow {
    pub tool: ToolId,
    pub allowed: Vec<bool>,
}

/// Builds the peer tool matrix over every listed tool.
///
/// With `only_peer` set the matrix has that one peer and keeps only the
/// tools it may use. The text filter applies to tool names, descriptions
/// and server names as in [`catalog`].
pub fn peer_matrix<'a>(
    builtin: &BuiltinTools,
    servers: &[McpServer],
    peers: &'a [PeerTools],
    only_peer: Option<&str>,
    filter: &Filter,
) -> PeerMatrix<'a> {
    let peers: Vec<&PeerTools> = match only_peer {
        Some(key) => peers.iter().filter(|peer| peer.key == key).collect(),
        None => peers.iter().collect(),
    };
    let builtin_tools = builtin
        .harness_tools
        .iter()
        .chain(&builtin.app_tools)
        .filter(|tool| filter.matches_tool(tool))
        .map(|tool| ToolId::builtin(&tool.name));
    let server_tools = servers.iter().flat_map(|server| {
        let name_matches = filter.matches(&server.name);
        server
            .tools
            .iter()
            .filter(move |tool| name_matches || filter.matches_tool(tool))
            .map(|tool| ToolId::mcp(&server.name, &tool.name))
    });
    let rows = builtin_tools
        .chain(server_tools)
        .filter_map(|tool| {
            let allowed: Vec<bool> = peers.iter().map(|peer| peer.allows(&tool)).collect();
            if only_peer.is_some() && !allowed.iter().any(|a| *a) {
                return None;
            }
            Some(MatrixRow { tool, allowed })
        })
        .collect();
    PeerMatrix { peers, rows }
}

#[cfg(test)]
mod tests {
    use loa_adapter::McpTransport;

    use super::*;
    use crate::model::{ServerKind, ServerStatus, ToolRule};

    fn tool(name: &str, description: &str) -> ToolInfo {
        ToolInfo::new(name, description)
    }

    fn catalog<'a>(
        builtin: &'a BuiltinTools,
        servers: &'a [McpServer],
        disabled: &BTreeSet<ToolId>,
        filter: &Filter,
    ) -> Catalog<'a> {
        let search = Search::new(builtin, servers);
        Rows::new(builtin, servers, &search, disabled, filter).catalog(builtin, servers)
    }

    fn builtin() -> BuiltinTools {
        BuiltinTools {
            harness: "omp".into(),
            harness_tools: vec![
                tool("bash", "Run a shell command"),
                tool("read", "Read a file"),
            ],
            app_tools: vec![
                tool("project_search", "Search sessions in the project"),
                tool("project_read", "Read a session of the project"),
                tool("sandbox_task", "Run a task in a sandbox"),
            ],
            harness_note: None,
        }
    }

    fn server(name: &str, tools: &[(&str, &str)]) -> McpServer {
        McpServer {
            name: name.into(),
            transport: McpTransport::Http {
                url: format!("http://localhost/{name}"),
                headers: Default::default(),
            },
            status: ServerStatus::Running,
            kind: ServerKind::External,
            tools: tools.iter().map(|(n, d)| tool(n, d)).collect(),
        }
    }

    fn servers() -> Vec<McpServer> {
        vec![
            server(
                "remote",
                &[
                    ("remote_bash", "Run a command on an SSH host"),
                    ("remote_file", "Files on an SSH host"),
                ],
            ),
            server(
                "browser",
                &[
                    ("surface_screenshot", "Capture"),
                    ("surface_click", "Click"),
                ],
            ),
            // Same tool name as a built in: must stay under its server.
            server("files", &[("read", "Read via MCP")]),
        ]
    }

    fn names(rows: &[ToolRow<'_>]) -> Vec<String> {
        rows.iter().map(|row| row.info.name.clone()).collect()
    }

    #[test]
    fn mcp_tools_stay_under_their_server_and_out_of_built_in() {
        let builtin = builtin();
        let servers = servers();
        let catalog = catalog(&builtin, &servers, &BTreeSet::new(), &Filter::default());
        assert_eq!(catalog.builtin.len(), 2);
        assert_eq!(catalog.builtin[0].group, BuiltinGroup::Harness);
        assert_eq!(catalog.builtin[0].title, "omp");
        assert_eq!(names(&catalog.builtin[0].tools), ["bash", "read"]);
        assert_eq!(catalog.builtin[1].group, BuiltinGroup::App);
        assert_eq!(
            names(&catalog.builtin[1].tools),
            ["project_search", "project_read", "sandbox_task"]
        );
        assert_eq!(catalog.servers.len(), 3);
        assert_eq!(
            names(&catalog.servers[0].tools),
            ["remote_bash", "remote_file"]
        );
        assert_eq!(catalog.servers[2].tools[0].id, ToolId::mcp("files", "read"));
        assert_eq!(catalog.builtin[0].tools[1].id, ToolId::builtin("read"));
    }

    #[test]
    fn disabling_a_builtin_does_not_disable_a_same_named_mcp_tool() {
        let builtin = builtin();
        let servers = servers();
        let disabled = BTreeSet::from([ToolId::builtin("read")]);
        let catalog = catalog(&builtin, &servers, &disabled, &Filter::default());
        assert!(!catalog.builtin[0].tools[1].enabled);
        assert!(catalog.servers[2].tools[0].enabled);
    }

    #[test]
    fn server_toggle_is_on_off_or_mixed() {
        let servers = servers();
        let remote = &servers[0];
        assert_eq!(server_toggle(remote, &BTreeSet::new()), Toggle::On);
        let one = BTreeSet::from([ToolId::mcp("remote", "remote_bash")]);
        assert_eq!(server_toggle(remote, &one), Toggle::Mixed);
        let both = BTreeSet::from([
            ToolId::mcp("remote", "remote_bash"),
            ToolId::mcp("remote", "remote_file"),
        ]);
        assert_eq!(server_toggle(remote, &both), Toggle::Off);
        // A disabled tool of another server does not count.
        let other = BTreeSet::from([ToolId::mcp("browser", "remote_bash")]);
        assert_eq!(server_toggle(remote, &other), Toggle::On);
        assert!(Toggle::Mixed.next_enabled());
        assert!(Toggle::Off.next_enabled());
        assert!(!Toggle::On.next_enabled());
    }

    #[test]
    fn filter_matches_name_description_and_server_name() {
        let builtin = builtin();
        let servers = servers();
        let none = BTreeSet::new();

        let by_name = catalog(&builtin, &servers, &none, &Filter::new("PROJECT_"));
        assert_eq!(
            by_name.builtin.len(),
            1,
            "harness group with no match is hidden"
        );
        assert_eq!(
            names(&by_name.builtin[0].tools),
            ["project_search", "project_read"]
        );
        assert!(by_name.servers.is_empty());

        let by_description = catalog(&builtin, &servers, &none, &Filter::new("ssh host"));
        assert!(by_description.builtin.is_empty());
        assert_eq!(by_description.servers.len(), 1);
        assert_eq!(
            names(&by_description.servers[0].tools),
            ["remote_bash", "remote_file"]
        );

        let by_server = catalog(&builtin, &servers, &none, &Filter::new("brow"));
        assert_eq!(by_server.servers.len(), 1);
        assert_eq!(
            names(&by_server.servers[0].tools),
            ["surface_screenshot", "surface_click"]
        );

        let partial = catalog(&builtin, &servers, &none, &Filter::new("click"));
        assert_eq!(names(&partial.servers[0].tools), ["surface_click"]);
    }

    #[test]
    fn filtered_server_toggle_still_reflects_all_tools() {
        let builtin = builtin();
        let servers = servers();
        let disabled = BTreeSet::from([ToolId::mcp("browser", "surface_screenshot")]);
        let catalog = catalog(&builtin, &servers, &disabled, &Filter::new("click"));
        assert_eq!(catalog.servers[0].toggle, Toggle::Mixed);
        assert!(catalog.servers[0].tools[0].enabled);
    }

    fn peers() -> Vec<PeerTools> {
        vec![
            PeerTools {
                key: "k-alice".into(),
                name: "alice".into(),
                rules: vec![ToolRule::All],
            },
            PeerTools {
                key: "k-bob".into(),
                name: "bob".into(),
                rules: vec![
                    ToolRule::Tool(ToolId::builtin("read")),
                    ToolRule::Server("browser".into()),
                ],
            },
            PeerTools {
                key: "k-carol".into(),
                name: "carol".into(),
                rules: vec![ToolRule::AllBuiltin],
            },
        ]
    }

    #[test]
    fn peer_matrix_follows_grant_rules() {
        let builtin = builtin();
        let servers = servers();
        let peers = peers();
        let matrix = peer_matrix(&builtin, &servers, &peers, None, &Filter::default());
        assert_eq!(matrix.peers.len(), 3);
        let row = |id: ToolId| {
            matrix
                .rows
                .iter()
                .find(|r| r.tool == id)
                .unwrap()
                .allowed
                .clone()
        };
        assert_eq!(row(ToolId::builtin("bash")), [true, false, true]);
        assert_eq!(row(ToolId::builtin("read")), [true, true, true]);
        assert_eq!(row(ToolId::mcp("files", "read")), [true, false, false]);
        assert_eq!(
            row(ToolId::mcp("browser", "surface_click")),
            [true, true, false]
        );
        assert_eq!(
            row(ToolId::mcp("remote", "remote_bash")),
            [true, false, false]
        );
        assert_eq!(matrix.rows.len(), 5 + 5);
    }

    #[test]
    fn peer_filter_keeps_only_that_peers_allowed_tools() {
        let builtin = builtin();
        let servers = servers();
        let peers = peers();
        let matrix = peer_matrix(
            &builtin,
            &servers,
            &peers,
            Some("k-bob"),
            &Filter::default(),
        );
        assert_eq!(matrix.peers.len(), 1);
        assert_eq!(matrix.peers[0].name, "bob");
        let tools: Vec<_> = matrix.rows.iter().map(|r| r.tool.clone()).collect();
        assert_eq!(
            tools,
            [
                ToolId::builtin("read"),
                ToolId::mcp("browser", "surface_screenshot"),
                ToolId::mcp("browser", "surface_click"),
            ]
        );

        let filtered = peer_matrix(
            &builtin,
            &servers,
            &peers,
            Some("k-bob"),
            &Filter::new("click"),
        );
        assert_eq!(filtered.rows.len(), 1);
    }
}
