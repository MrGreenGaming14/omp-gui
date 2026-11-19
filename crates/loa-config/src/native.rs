//! The port adapters implement to write the central config into a harness's
//! native config files.

use crate::{Config, McpServer, ProjectSettings};
use std::collections::BTreeSet;
use std::path::Path;

/// The slice of the central config one harness needs, resolved for one project.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeConfig<'a> {
    /// Harness name, as in `loa_adapter::Adapter::harness`.
    pub harness: &'a str,
    /// Project id, when the session belongs to one.
    pub project: Option<&'a str>,
    /// Enabled MCP servers, by name.
    pub mcp_servers: Vec<(&'a str, &'a McpServer)>,
    /// Built in tools switched off in the project.
    pub disabled_tools: &'a BTreeSet<String>,
    /// The project's settings (MCP tool toggles, instructions), when it has any.
    pub project_settings: Option<&'a ProjectSettings>,
}

impl NativeConfig<'_> {
    /// Whether `tool` of MCP server `server` is on in the project.
    pub fn mcp_tool_enabled(&self, server: &str, tool: &str) -> bool {
        self.project_settings.is_none_or(|p| p.mcp_tool_enabled(server, tool))
    }

    /// Disabled tools of MCP server `server`.
    pub fn disabled_mcp_tools(&self, server: &str) -> impl Iterator<Item = &str> {
        self.project_settings
            .and_then(|p| p.disabled_mcp_tools.get(server))
            .into_iter()
            .flatten()
            .map(String::as_str)
    }
}

static NO_TOOLS: BTreeSet<String> = BTreeSet::new();

impl Config {
    /// What `harness` must be told about MCP servers and tool toggles for a
    /// session in `project`.
    pub fn native_view<'a>(&'a self, harness: &'a str, project: Option<&'a str>) -> NativeConfig<'a> {
        let project_settings = project.and_then(|id| self.projects.get(id));
        NativeConfig {
            harness,
            project,
            mcp_servers: self
                .mcp_servers
                .iter()
                .filter(|(_, server)| server.enabled)
                .map(|(name, server)| (name.as_str(), server))
                .collect(),
            disabled_tools: project_settings.map_or(&NO_TOOLS, |p| &p.disabled_tools),
            project_settings,
        }
    }
}

/// Failure to translate the central config into a harness's native config.
#[derive(Debug, thiserror::Error)]
pub enum NativeConfigError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The existing native file couldn't be parsed or updated.
    #[error("malformed native config {path}: {detail}")]
    Format { path: String, detail: String },
    /// The harness can't express part of the config (for example per tool
    /// toggles of MCP servers).
    #[error("{harness} can't express {what}")]
    Unsupported { harness: &'static str, what: &'static str },
}

/// Implemented by each harness adapter: writes MCP servers and disabled tools
/// from the central config into the harness's own config files, leaving the
/// rest of those files as they were. The central config stays the source of
/// truth; the native files are regenerated from it before a session starts.
pub trait NativeConfigWriter: Send + Sync {
    /// The harness this writer serves.
    fn harness(&self) -> &'static str;

    /// Writes `config` for a session running in `cwd` (where project scoped
    /// native files such as `.mcp.json` live). Global native files come from
    /// paths the writer was constructed with.
    fn write(&self, cwd: &Path, config: &NativeConfig<'_>) -> Result<(), NativeConfigError>;
}
