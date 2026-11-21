//! What the Tools panel lists, built from the services: MCP servers with
//! their tools, import candidates, and the tools each peer's grants allow.

use std::collections::BTreeMap;

use loa_contracts::harness::McpTransport;
use loa_trust::{Action, IssuedRecord, Resource};
use loa_mcp::import::{App, Found, ImportReport, Outcome};
use loa_mcp::{ServerView, Status, central};
use loa_view::panel;

/// Prefix of the tools surface servers expose.
const SURFACE_TOOL_PREFIX: &str = "surface_";

/// Characters of a tool description the panel shows.
const DESCRIPTION_CHARS: usize = 160;

/// The first line of a tool's description, shortened for a list row.
pub(super) fn short_description(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
    match line.char_indices().nth(DESCRIPTION_CHARS) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

/// A server the manager runs, as a panel row. Servers whose tools are
/// `surface_*` tools are marked as surface servers.
pub(super) fn server(view: &ServerView) -> panel::McpServer {
    let status = match &view.status {
        Status::Starting => panel::ServerStatus::Starting,
        Status::Running => panel::ServerStatus::Running,
        Status::Stopped => panel::ServerStatus::Stopped,
        Status::Error(message) => panel::ServerStatus::Error(message.clone()),
    };
    let surface = view.tools.iter().any(|tool| tool.tool.name.starts_with(SURFACE_TOOL_PREFIX));
    panel::McpServer {
        name: view.spec.name.clone(),
        transport: central::transport(&view.spec),
        status,
        kind: if surface { panel::ServerKind::Surface } else { panel::ServerKind::External },
        tools: view
            .tools
            .iter()
            .map(|tool| panel::ToolInfo::new(tool.tool.name.to_string(), short_description(tool.tool.description.as_deref().unwrap_or_default())))
            .collect(),
    }
}

/// An app hosted server row.
pub(super) fn app_server(
    name: &str,
    transport: McpTransport,
    status: panel::ServerStatus,
    tools: Vec<panel::ToolInfo>,
) -> panel::McpServer {
    panel::McpServer { name: name.to_owned(), transport, status, kind: panel::ServerKind::AppHosted, tools }
}

pub(super) fn import_source(app: App) -> panel::ImportSource {
    match app {
        App::Claude => panel::ImportSource::Claude,
        App::Cursor => panel::ImportSource::Cursor,
        App::Codex => panel::ImportSource::Codex,
    }
}

/// The usable entries of a scan as import candidates.
pub(super) fn candidates(found: &[Found]) -> Vec<panel::ImportCandidate> {
    found
        .iter()
        .filter_map(|entry| {
            let spec = entry.spec.as_ref().ok()?;
            Some(panel::ImportCandidate {
                source: import_source(entry.app),
                path: entry.file.display().to_string(),
                server: central::session_spec(spec),
            })
        })
        .collect()
}

/// The scanned entries the user picked.
pub(super) fn picked(found: &[Found], chosen: &[panel::ImportCandidate]) -> Vec<Found> {
    found
        .iter()
        .filter(|entry| {
            chosen.iter().any(|candidate| {
                candidate.source == import_source(entry.app)
                    && candidate.path == entry.file.display().to_string()
                    && candidate.server.name == entry.name
            })
        })
        .cloned()
        .collect()
}

/// What happened to each imported entry, for the import list.
pub(super) fn outcomes(report: &ImportReport) -> Vec<(panel::ImportSource, String, panel::ImportOutcome)> {
    report
        .entries
        .iter()
        .map(|entry| {
            let outcome = match &entry.outcome {
                Outcome::Imported(_) => panel::ImportOutcome::Imported,
                Outcome::Duplicate { existing } => panel::ImportOutcome::Skipped(format!("already registered as {existing}")),
                Outcome::Unsupported { reason } => panel::ImportOutcome::Failed(reason.clone()),
            };
            (import_source(entry.app), entry.name.clone(), outcome)
        })
        .collect()
}

/// Per peer, the tools its live grants let it have the agent use, as the
/// grant checks decide them:
/// - every tool needs drive on a session (or a turn, project, future sessions)
/// - `remote` tools also need an SSH grant on a known host: exec for
///   `remote_bash`, read for the others
/// - `surface_*` tools need a grant on a surface of their server: view for
///   `surface_screenshot`, drive otherwise
pub(super) fn peer_tools(
    issued: &[IssuedRecord],
    servers: &[panel::McpServer],
    hosts: &[String],
    now_ms: i64,
) -> Vec<panel::PeerTools> {
    let mut scopes: BTreeMap<String, Vec<&loa_trust::Scope>> = BTreeMap::new();
    for record in issued {
        let live = record.revoked_ms.is_none() && record.until_ms.is_none_or(|until| until > now_ms);
        if let (true, Some(granted)) = (live, &record.scopes) {
            scopes.entry(record.holder.to_string()).or_default().extend(granted);
        }
    }
    scopes
        .into_iter()
        .map(|(key, scopes)| {
            let may = |action: Action, resource: &dyn Fn(&Resource) -> bool| {
                scopes.iter().any(|scope| scope.actions.contains(&action) && resource(&scope.resource))
            };
            let drive = may(Action::Drive, &|resource| {
                matches!(
                    resource,
                    Resource::Session { .. } | Resource::Prompt { .. } | Resource::Project { .. } | Resource::FutureSessions { .. }
                )
            });
            let mut rules = Vec::new();
            if drive {
                rules.push(panel::ToolRule::AllBuiltin);
                for server in servers {
                    for tool in &server.tools {
                        let allowed = if server.name == loa_trust::REMOTE_SERVER {
                            let action = if tool.name == loa_remote::tools::REMOTE_BASH { Action::Exec } else { Action::Read };
                            may(action, &|resource| matches!(resource, Resource::Ssh { host } if hosts.contains(host)))
                        } else if tool.name.starts_with(SURFACE_TOOL_PREFIX) {
                            let action = if tool.name == "surface_screenshot" { Action::View } else { Action::Drive };
                            let prefix = format!("surface://{}/", server.name);
                            may(action, &|resource| matches!(resource, Resource::Surface { uri } if uri.starts_with(&prefix)))
                        } else {
                            true
                        };
                        if allowed {
                            rules.push(panel::ToolRule::Tool(panel::ToolId::mcp(&server.name, &tool.name)));
                        }
                    }
                }
            }
            let name = key[..key.len().min(8)].to_owned();
            panel::PeerTools { key, name, rules }
        })
        .collect()
}
