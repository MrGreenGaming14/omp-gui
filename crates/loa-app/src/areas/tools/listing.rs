//! What the Tools panel lists, built from the services: MCP servers with
//! their tools, import candidates, and the tools each peer's grants allow.

use std::collections::BTreeMap;

use loa_contracts::harness::McpTransport;
use loa_trust::{Action, IssuedRecord, Resource};
use loa_mcp::import::{Found, ImportReport};
use loa_mcp::{ServerView, central};
use loa_ui_panel::tools;

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

/// A server the manager runs, as a panel row.
pub(super) fn server(view: &ServerView) -> tools::McpServer {
    let surface = view.tools.iter().any(|tool| tool.tool.name.starts_with(SURFACE_TOOL_PREFIX));
    tools::McpServer {
        name: view.spec.name.clone(),
        transport: central::transport(&view.spec),
        status: (&view.status).into(),
        kind: if surface { tools::ServerKind::Surface } else { tools::ServerKind::External },
        tools: view
            .tools
            .iter()
            .map(|tool| tools::ToolInfo::new(tool.tool.name.to_string(), short_description(tool.tool.description.as_deref().unwrap_or_default())))
            .collect(),
    }
}

/// An app hosted server row.
pub(super) fn app_server(
    name: &str,
    transport: McpTransport,
    status: tools::ServerStatus,
    tools: Vec<tools::ToolInfo>,
) -> tools::McpServer {
    tools::McpServer { name: name.to_owned(), transport, status, kind: tools::ServerKind::AppHosted, tools }
}

/// The usable entries of a scan as import candidates.
pub(super) fn candidates(found: &[Found]) -> Vec<tools::ImportCandidate> {
    found
        .iter()
        .filter_map(|entry| {
            let spec = entry.spec.as_ref().ok()?;
            Some(tools::ImportCandidate {
                source: entry.app.into(),
                path: entry.file.display().to_string(),
                server: central::session_spec(spec),
            })
        })
        .collect()
}

/// The scanned entries the user picked.
pub(super) fn picked(found: &[Found], chosen: &[tools::ImportCandidate]) -> Vec<Found> {
    found
        .iter()
        .filter(|entry| {
            chosen.iter().any(|candidate| {
                candidate.source == entry.app.into()
                    && candidate.path == entry.file.display().to_string()
                    && candidate.server.name == entry.name
            })
        })
        .cloned()
        .collect()
}

/// What happened to each imported entry, for the import list.
pub(super) fn outcomes(report: &ImportReport) -> Vec<(tools::ImportSource, String, tools::ImportOutcome)> {
    report
        .entries
        .iter()
        .map(|entry| (entry.app.into(), entry.name.clone(), (&entry.outcome).into()))
        .collect()
}

/// Per peer, the tools its live grants let it have the agent use, as the
/// grant checks decide them.
pub(super) fn peer_tools(
    issued: &[IssuedRecord],
    servers: &[tools::McpServer],
    hosts: &[String],
    now_ms: i64,
) -> Vec<tools::PeerTools> {
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
                rules.push(tools::ToolRule::AllBuiltin);
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
                            rules.push(tools::ToolRule::Tool(tools::ToolId::mcp(&server.name, &tool.name)));
                        }
                    }
                }
            }
            let name = key[..key.len().min(8)].to_owned();
            tools::PeerTools { key, name, rules }
        })
        .collect()
}
