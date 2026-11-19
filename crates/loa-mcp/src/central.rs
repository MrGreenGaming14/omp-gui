//! The app's central config as the server store, and the per project tool
//! toggles it holds.

use std::collections::BTreeMap;

use async_trait::async_trait;
use loa_adapter::{McpServerSpec, McpTransport};
use loa_config::{ConfigStore, McpServer, ProjectSettings};

use crate::registry::{ServerStore, StoreError};
use crate::spec::{HttpSpec, ServerSpec, StdioSpec, Transport};
use crate::ToolFilter;

/// A registered server from the central config. Specs that don't validate
/// (a hand edited name) are still returned; the manager refuses to run them.
pub fn from_config(name: &str, server: &McpServer) -> ServerSpec {
    let transport = match &server.transport {
        McpTransport::Stdio { command, args, env } => Transport::Stdio(StdioSpec {
            command: command.clone(),
            args: args.clone(),
            env: env.clone(),
            cwd: None,
        }),
        McpTransport::Http { url, headers } => Transport::Http(HttpSpec { url: url.clone(), headers: headers.clone() }),
    };
    ServerSpec { name: name.to_owned(), enabled: server.enabled, transport }
}

/// The adapter contract's transport for a spec. The working directory of a
/// stdio server has no place there and is dropped.
pub fn transport(spec: &ServerSpec) -> McpTransport {
    match &spec.transport {
        Transport::Stdio(stdio) => {
            McpTransport::Stdio { command: stdio.command.clone(), args: stdio.args.clone(), env: stdio.env.clone() }
        }
        Transport::Http(http) => McpTransport::Http { url: http.url.clone(), headers: http.headers.clone() },
    }
}

/// The central config entry for a spec.
pub fn to_config(spec: &ServerSpec) -> McpServer {
    McpServer { transport: transport(spec), enabled: spec.enabled }
}

/// The server as a harness is told about it in its `SessionConfig`.
pub fn session_spec(spec: &ServerSpec) -> McpServerSpec {
    McpServerSpec { name: spec.name.clone(), transport: transport(spec) }
}

/// A user typed server from the Tools panel's add form, which speaks the
/// adapter contract's types.
pub fn from_session_spec(spec: &McpServerSpec) -> ServerSpec {
    from_config(&spec.name, &McpServer { transport: spec.transport.clone(), enabled: true })
}

/// [`ServerStore`] over the `mcp_servers` table of the central config. Each
/// save reads the file again and replaces only that table, so other keys
/// written in between survive.
#[derive(Clone, Debug)]
pub struct ConfigServers {
    store: ConfigStore,
}

impl ConfigServers {
    pub fn new(store: ConfigStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ServerStore for ConfigServers {
    async fn load(&self) -> Result<Vec<ServerSpec>, StoreError> {
        let store = self.store.clone();
        let loaded = tokio::task::spawn_blocking(move || store.load()).await??;
        Ok(loaded.config.mcp_servers.iter().map(|(name, server)| from_config(name, server)).collect())
    }

    async fn save(&self, servers: &[ServerSpec]) -> Result<(), StoreError> {
        let store = self.store.clone();
        let table: BTreeMap<String, McpServer> =
            servers.iter().map(|spec| (spec.name.clone(), to_config(spec))).collect();
        tokio::task::spawn_blocking(move || {
            let mut config = store.load()?.config;
            config.mcp_servers = table;
            store.save(&config)
        })
        .await??;
        Ok(())
    }
}

/// The MCP tool toggles of one project as a [`ToolFilter`].
#[derive(Clone, Debug, Default)]
pub struct ProjectFilter {
    settings: ProjectSettings,
}

impl ProjectFilter {
    pub fn new(settings: ProjectSettings) -> Self {
        Self { settings }
    }
}

impl ToolFilter for ProjectFilter {
    fn allows(&self, server: &str, tool: &str) -> bool {
        self.settings.mcp_tool_enabled(server, tool)
    }
}

/// The name a harness that connects to MCP servers itself gives an MCP
/// tool (Claude Code's `mcp__server__tool`).
pub fn harness_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// What a session's `disabled_tools` holds for a project: its built in
/// tools as they are, MCP tools as [`harness_tool_name`]s.
pub fn session_disabled_tools(settings: &ProjectSettings) -> Vec<String> {
    let mut names: Vec<String> = settings.disabled_tools.iter().cloned().collect();
    for (server, tools) in &settings.disabled_mcp_tools {
        names.extend(tools.iter().map(|tool| harness_tool_name(server, tool)));
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_central_config_stores_servers_without_touching_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::in_dir(dir.path());
        let mut config = store.load().unwrap().config;
        config.harness.default = "claude".into();
        store.save(&config).unwrap();

        let servers = ConfigServers::new(store.clone());
        assert!(servers.load().await.unwrap().is_empty());
        let fs = ServerSpec::new("fs", Transport::Stdio(StdioSpec::parse("K=v mcp-fs /tmp").unwrap())).unwrap();
        let web = ServerSpec {
            enabled: false,
            ..ServerSpec::new("web", Transport::Http(HttpSpec::new("https://mcp.example.com/mcp", []).unwrap())).unwrap()
        };
        servers.save(&[fs.clone(), web.clone()]).await.unwrap();
        assert_eq!(servers.load().await.unwrap(), [fs.clone(), web]);
        assert_eq!(store.load().unwrap().config.harness.default, "claude");

        let session = session_spec(&fs);
        assert_eq!(session.name, "fs");
        assert!(matches!(&session.transport, McpTransport::Stdio { command, env, .. } if command == "mcp-fs" && env["K"] == "v"));
        assert_eq!(from_session_spec(&session), fs);
    }

    #[test]
    fn project_toggles_filter_and_name_disabled_tools() {
        let mut settings = ProjectSettings::default();
        settings.disabled_tools.insert("bash".into());
        settings.disabled_mcp_tools.entry("git".into()).or_default().insert("git_push".into());
        let filter = ProjectFilter::new(settings.clone());
        assert!(!filter.allows("git", "git_push"));
        assert!(filter.allows("git", "git_log"));
        assert_eq!(session_disabled_tools(&settings), ["bash", "mcp__git__git_push"]);
    }
}
