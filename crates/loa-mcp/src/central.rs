//! The app's central config as the server store, and its per project tool toggles.

use std::collections::BTreeMap;

use async_trait::async_trait;
use loa_settings::app::{ConfigStore, McpServer, ProjectSettings};
use loa_contracts::harness::{McpServerSpec, McpTransport};

use crate::ToolFilter;
use crate::registry::{ServerStore, StoreError};
use crate::spec::{HttpSpec, ServerSpec, StdioSpec, Transport};

/// A server of the central config; one that doesn't validate is still returned and the manager refuses to run it.
pub fn from_config(name: &str, server: &McpServer) -> ServerSpec {
    let transport = match &server.transport {
        McpTransport::Stdio { command, args, env } => Transport::Stdio(StdioSpec { command: command.clone(), args: args.clone(), env: env.clone(), cwd: None }),
        McpTransport::Http { url, headers } => Transport::Http(HttpSpec { url: url.clone(), headers: headers.clone() }),
    };
    ServerSpec { name: name.to_owned(), enabled: server.enabled, transport }
}

/// The contract's transport for a spec; a stdio server's working directory is dropped.
pub fn transport(spec: &ServerSpec) -> McpTransport {
    match &spec.transport {
        Transport::Stdio(stdio) => McpTransport::Stdio { command: stdio.command.clone(), args: stdio.args.clone(), env: stdio.env.clone() },
        Transport::Http(http) => McpTransport::Http { url: http.url.clone(), headers: http.headers.clone() },
    }
}

pub fn to_config(spec: &ServerSpec) -> McpServer {
    McpServer { transport: transport(spec), enabled: spec.enabled }
}

/// The server as a harness is told about it in its `SessionConfig`.
pub fn session_spec(spec: &ServerSpec) -> McpServerSpec {
    McpServerSpec { name: spec.name.clone(), transport: transport(spec) }
}

/// A server typed in the Tools panel's add form.
pub fn from_session_spec(spec: &McpServerSpec) -> ServerSpec {
    from_config(&spec.name, &McpServer { transport: spec.transport.clone(), enabled: true })
}

/// [`ServerStore`] over the `mcp_servers` table of the central config; a
/// save replaces only that table, so other keys written in between survive.
#[derive(Clone, Debug)]
pub struct ConfigServers(ConfigStore);

impl ConfigServers {
    pub fn new(store: ConfigStore) -> Self {
        Self(store)
    }
}

#[async_trait]
impl ServerStore for ConfigServers {
    async fn load(&self) -> Result<Vec<ServerSpec>, StoreError> {
        let store = self.0.clone();
        let config = tokio::task::spawn_blocking(move || store.load()).await??;
        Ok(config.mcp_servers.iter().map(|(name, server)| from_config(name, server)).collect())
    }

    async fn save(&self, servers: &[ServerSpec]) -> Result<(), StoreError> {
        let store = self.0.clone();
        let table: BTreeMap<String, McpServer> = servers.iter().map(|spec| (spec.name.clone(), to_config(spec))).collect();
        tokio::task::spawn_blocking(move || store.edit(|config| config.mcp_servers = table)).await??;
        Ok(())
    }
}

/// The MCP tool toggles of one project.
#[derive(Clone, Debug, Default)]
pub struct ProjectFilter(ProjectSettings);

impl ProjectFilter {
    pub fn new(settings: ProjectSettings) -> Self {
        Self(settings)
    }
}

impl ToolFilter for ProjectFilter {
    fn allows(&self, server: &str, tool: &str) -> bool {
        self.0.mcp_tool_enabled(server, tool)
    }
}

/// What a harness that connects to MCP servers itself calls an MCP tool (Claude Code's `mcp__server__tool`).
pub fn harness_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// A session's `disabled_tools` for a project: built in tools as they are, MCP tools as [`harness_tool_name`]s.
pub fn session_disabled_tools(settings: &ProjectSettings) -> Vec<String> {
    let mcp = settings.disabled_mcp_tools.iter().flat_map(|(server, tools)| tools.iter().map(|tool| harness_tool_name(server, tool)));
    settings.disabled_tools.iter().cloned().chain(mcp).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_central_config_stores_servers_without_touching_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::in_dir(dir.path());
        let mut config = store.load().unwrap();
        config.harness.default = "claude".into();
        store.save(&config).unwrap();

        let servers = ConfigServers::new(store.clone());
        assert!(servers.load().await.unwrap().is_empty());
        let fs = ServerSpec::new("fs", Transport::Stdio(StdioSpec::parse("K=v mcp-fs /tmp").unwrap())).unwrap();
        let web = ServerSpec::new("web", Transport::Http(HttpSpec::new("https://mcp.example.com/mcp", []).unwrap())).unwrap();
        let web = ServerSpec { enabled: false, ..web };
        servers.save(&[fs.clone(), web.clone()]).await.unwrap();
        assert_eq!(servers.load().await.unwrap(), [fs.clone(), web]);
        assert_eq!(store.load().unwrap().harness.default, "claude");

        let session = session_spec(&fs);
        assert!(matches!(&session.transport, McpTransport::Stdio { command, env, .. } if command == "mcp-fs" && env["K"] == "v"));
        assert_eq!(from_session_spec(&session), fs);

        let mut settings = ProjectSettings::default();
        settings.disabled_tools.insert("bash".into());
        settings.disabled_mcp_tools.entry("git".into()).or_default().insert("git_push".into());
        let filter = ProjectFilter::new(settings.clone());
        assert!(!filter.allows("git", "git_push") && filter.allows("git", "git_log"));
        assert_eq!(session_disabled_tools(&settings), ["bash", "mcp__git__git_push"]);
    }
}
