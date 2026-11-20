//! The Peers panel (`loa-ui-peers`) over a [`Node`]: the snapshot it shows
//! and the effects of its events. The app area only forwards: it puts
//! [`data`] into the panel and runs [`apply`] for each panel event.

use crate::{
    Availability, DiscoverySource, Endpoint, GrantDirection, GrantInfo, KeySource, NearbySource, Node, NodeError,
    RemoteView, Transport,
};
use loa_core::NodeId as DomNodeId;
use loa_grants::{Action, GrantId, Resource, Scope};
use loa_ui_peers as panel;
use std::sync::Arc;

/// A grant resource as the panel's picker id.
pub fn resource_id(resource: &Resource) -> String {
    match resource {
        Resource::Session { id } => format!("session:{id}"),
        Resource::Prompt { session, turn } => format!("prompt:{}:{session}", turn.0),
        Resource::Project { id } => format!("project:{id}"),
        Resource::FutureSessions { .. } => "future".to_owned(),
        Resource::Ssh { host } => format!("ssh:{host}"),
        Resource::Surface { uri } => format!("surface:{uri}"),
        Resource::Sandbox => "sandbox".to_owned(),
        Resource::Folder { path } => format!("folder:{path}"),
    }
}

/// The resource a picker id names. Future sessions start at `now_ms`.
pub fn parse_resource(id: &str, now_ms: i64) -> Option<Resource> {
    match id {
        "future" => return Some(Resource::FutureSessions { after_ms: now_ms }),
        "sandbox" => return Some(Resource::Sandbox),
        _ => {}
    }
    let (kind, rest) = id.split_once(':')?;
    Some(match kind {
        "session" => Resource::Session { id: rest.to_owned() },
        "prompt" => {
            let (turn, session) = rest.split_once(':')?;
            Resource::Prompt { session: session.to_owned(), turn: DomNodeId(turn.parse().ok()?) }
        }
        "project" => Resource::Project { id: rest.to_owned() },
        "ssh" => Resource::Ssh { host: rest.to_owned() },
        "surface" => Resource::Surface { uri: rest.to_owned() },
        "folder" => Resource::Folder { path: rest.to_owned() },
        _ => return None,
    })
}

fn scope_kind(resource: &Resource) -> panel::GrantScope {
    match resource {
        Resource::Session { .. } => panel::GrantScope::Session,
        Resource::Prompt { .. } => panel::GrantScope::Prompt,
        Resource::Project { .. } => panel::GrantScope::Project,
        Resource::FutureSessions { .. } => panel::GrantScope::FutureSessions,
        Resource::Ssh { .. } => panel::GrantScope::SshHost,
        Resource::Surface { .. } => panel::GrantScope::Surface,
        Resource::Sandbox => panel::GrantScope::Sandbox,
        Resource::Folder { .. } => panel::GrantScope::Folder,
    }
}

/// The actions a panel permission gives on a resource kind.
fn actions(permission: panel::Permission, scope: panel::GrantScope) -> Result<Vec<Action>, NodeError> {
    use panel::Permission as P;
    Ok(match (scope, permission) {
        (panel::GrantScope::Sandbox, P::Drive) => vec![Action::Exec],
        (panel::GrantScope::Sandbox, _) => {
            return Err(NodeError::Refused("a sandbox grant only runs tasks (drive)".into()));
        }
        (panel::GrantScope::SshHost | panel::GrantScope::Folder, P::View) => vec![Action::Read],
        (panel::GrantScope::Folder, P::Drive) => vec![Action::Read, Action::Write],
        (panel::GrantScope::SshHost, P::Drive) => vec![Action::Read, Action::Write, Action::Exec],
        (panel::GrantScope::SshHost | panel::GrantScope::Surface | panel::GrantScope::Folder, P::Suggest | P::Fork) => {
            return Err(NodeError::Refused(format!("{permission} does not apply to this resource")));
        }
        (_, P::View) => vec![Action::View],
        (_, P::Suggest) => vec![Action::Suggest],
        (_, P::Drive) => vec![Action::Drive],
        (_, P::Fork) => vec![Action::Fork],
    })
}

/// The strongest panel permission among `actions`.
fn permission(actions: &std::collections::BTreeSet<Action>) -> panel::Permission {
    if actions.iter().any(|a| matches!(a, Action::Drive | Action::Exec | Action::Write)) {
        panel::Permission::Drive
    } else if actions.contains(&Action::Suggest) {
        panel::Permission::Suggest
    } else if actions.contains(&Action::Fork) {
        panel::Permission::Fork
    } else {
        panel::Permission::View
    }
}

/// The scopes, end and re-share flag a panel grant request asks for.
pub fn grant_request(input: &panel::GrantInput, now_ms: i64) -> Result<(Vec<Scope>, Option<i64>), NodeError> {
    let resource = parse_resource(&input.resource.id, now_ms)
        .ok_or_else(|| NodeError::Refused(format!("unknown resource {}", input.resource.id)))?;
    let resource = match resource {
        Resource::Folder { path } => Resource::Folder { path: local_folder(&path)? },
        other => other,
    };
    let actions = actions(input.permission, scope_kind(&resource))?;
    let mut scopes = vec![Scope::new(resource.clone(), actions.clone())?];
    if input.resource.future_sessions && matches!(resource, Resource::Prompt { .. }) {
        scopes.push(Scope::new(Resource::FutureSessions { after_ms: now_ms }, actions)?);
    }
    let ttl_ms = i64::try_from(input.ttl_seconds).unwrap_or(i64::MAX / 2).saturating_mul(1000);
    Ok((scopes, Some(now_ms.saturating_add(ttl_ms))))
}

/// The real path of a folder on this node a grant may name: it must be an
/// absolute path to an existing folder; links are resolved, so the grant
/// names the folder they lead to.
pub fn local_folder(path: &str) -> Result<String, NodeError> {
    // Checked on this node, so not a peer's refusal.
    let error = |text: String| NodeError::Unavailable(format!("Can't share {text}"));
    let path = path.trim();
    if !std::path::Path::new(path).is_absolute() {
        return Err(error(format!("{path:?}: give the folder's full path")));
    }
    let real = loa_exec::canonicalize(path).map_err(|e| error(format!("{path}: {e}")))?;
    if !real.is_dir() {
        return Err(error(format!("{path}: not a folder")));
    }
    real.into_os_string().into_string().map_err(|_| error(format!("{path}: not a UTF-8 path")))
}

fn resource_label(resource: &Resource, labels: &[(Resource, String)]) -> String {
    let local = labels.iter().find(|(r, _)| match (r, resource) {
        (Resource::FutureSessions { .. }, Resource::FutureSessions { .. }) => true,
        (a, b) => a == b,
    });
    if let Some((_, label)) = local {
        return label.clone();
    }
    match resource {
        Resource::Session { id } => format!("Session {id}"),
        Resource::Prompt { session, turn } => format!("Prompt {} of session {session}", turn.0),
        Resource::Project { id } => format!("Project {id}"),
        Resource::FutureSessions { .. } => "Every future session".to_owned(),
        Resource::Ssh { host } => format!("SSH {host}"),
        Resource::Surface { uri } => uri.clone(),
        Resource::Sandbox => crate::SANDBOX_LABEL.to_owned(),
        Resource::Folder { path } => format!("Folder {path}"),
    }
}

fn grant(info: &GrantInfo, labels: &[(Resource, String)]) -> Option<panel::Grant> {
    let primary = info
        .scopes
        .iter()
        .find(|s| !matches!(s.resource, Resource::FutureSessions { .. }))
        .or_else(|| info.scopes.first())?;
    let future_sessions = matches!(primary.resource, Resource::Prompt { .. })
        && info.scopes.iter().any(|s| matches!(s.resource, Resource::FutureSessions { .. }));
    let secs = |ms: i64| u64::try_from(ms / 1000).unwrap_or(0);
    Some(panel::Grant {
        id: info.id.as_str().to_owned(),
        peer_key: info.peer.to_string(),
        direction: match info.direction {
            GrantDirection::Given => panel::GrantDirection::Given,
            GrantDirection::Received => panel::GrantDirection::Received,
        },
        resource: panel::GrantResource {
            id: resource_id(&primary.resource),
            label: resource_label(&primary.resource, labels),
            scope: scope_kind(&primary.resource),
            future_sessions,
        },
        permission: permission(&primary.actions),
        expires_at: info.until_ms.or(info.expires_ms).map(secs),
        renews_at: info.renews_at_ms.map(secs),
        reshare: info.reshare,
        ended: info.ended,
    })
}

fn availability(value: &Availability) -> (String, bool) {
    match value {
        Availability::Off(text) => (format!("off: {text}"), false),
        Availability::Active(text) => (text.clone(), true),
        Availability::Failed(text) => (format!("failed: {text}"), false),
    }
}

/// The panel's snapshot of `node` at `now_ms`.
pub fn data(node: &Node, now_ms: i64) -> panel::Data {
    let status = node.status();
    let (endpoint, endpoint_ok) = match &status.endpoint {
        Endpoint::Stopped => (String::new(), false),
        Endpoint::Starting => ("starting".to_owned(), false),
        Endpoint::Listening { udp, tcp } => {
            let udp: Vec<String> = udp.iter().map(ToString::to_string).collect();
            (format!("UDP {} · TCP {tcp}", udp.join(", ")), true)
        }
        Endpoint::Failed(error) => (format!("failed: {error}"), false),
    };
    let (lan, lan_ok) = availability(&status.mdns);
    let (tailscale, tailscale_ok) = availability(&status.tailscale);
    let card = panel::NodeCard {
        key: status.key.to_string(),
        name: status.name.clone(),
        key_source: match &status.key_source {
            KeySource::Keychain => "OS keychain".to_owned(),
            KeySource::File { path, keychain_error } => format!("{} (keychain unavailable: {keychain_error})", path.display()),
            KeySource::Memory { reason } => format!("in memory only ({reason})"),
        },
        endpoint,
        endpoint_ok,
        lan,
        lan_ok,
        tailscale,
        tailscale_ok,
        tailscale_available: node.config().tailscale.is_some() && endpoint_ok,
    };
    let secs = |ms: i64| u64::try_from(ms / 1000).unwrap_or(0);
    let peers_info = node.peers();
    let peers = peers_info
        .iter()
        .map(|peer| panel::Peer {
            key: peer.key.to_string(),
            name: peer.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| peer.key.fmt_short().to_string()),
            discovery: match peer.source {
                DiscoverySource::Mdns => panel::Discovery::Lan,
                DiscoverySource::Tailscale => panel::Discovery::Tailscale,
                DiscoverySource::Manual => panel::Discovery::AddedByKey,
            },
            presence: if peer.online { panel::Presence::Online } else { panel::Presence::Offline },
            last_seen: peer.last_seen_ms.map(secs),
            transport: match peer.transport {
                Some(Transport::Quic) => panel::Transport::Direct,
                Some(Transport::Relay) => panel::Transport::Relay,
                Some(Transport::Tcp) => panel::Transport::TcpFallback,
                None => panel::Transport::None,
            },
            app_version: peer.app_version.clone().unwrap_or_default(),
            shared_protocol: peer.shared_protocol,
        })
        .collect();
    let nearby = node
        .nearby()
        .into_iter()
        .map(|near| panel::Peer {
            key: near.key.to_string(),
            name: near.name.clone().unwrap_or_else(|| near.key.fmt_short().to_string()),
            discovery: match near.source {
                NearbySource::Mdns => panel::Discovery::Lan,
                NearbySource::Tailscale => panel::Discovery::Tailscale,
                NearbySource::Incoming => panel::Discovery::Incoming,
            },
            presence: panel::Presence::Online,
            last_seen: Some(secs(near.seen_ms)),
            transport: panel::Transport::None,
            app_version: near.app_version.clone().unwrap_or_default(),
            shared_protocol: None,
        })
        .collect();
    let labels = node.resources();
    // Received grants name the peer's sessions: label them with the titles it listed.
    let mut known = labels.clone();
    known.extend(
        node.remote_sessions()
            .into_iter()
            .filter(|s| !s.title.is_empty())
            .map(|s| (Resource::Session { id: s.id }, s.title)),
    );
    let grants = node.grants().iter().filter_map(|info| grant(info, &known)).collect();
    let resources = labels
        .iter()
        .map(|(resource, label)| panel::ResourceChoice {
            id: resource_id(resource),
            label: label.clone(),
            scope: scope_kind(resource),
        })
        .collect();
    let remote_sessions = node
        .remote_sessions()
        .into_iter()
        .map(|session| {
            let online = peers_info.iter().any(|p| p.key == session.peer && p.online);
            let access = if session.actions.is_empty() {
                "some prompts".to_owned()
            } else {
                session.actions.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(", ")
            };
            panel::RemoteItem {
                peer_key: session.peer.to_string(),
                peer_name: node.peer_name(&session.peer),
                title: if session.title.is_empty() { format!("Session {}", session.id) } else { session.title.clone() },
                session: session.id,
                harness: session.harness,
                access,
                can_drive: session.actions.contains(&Action::Drive),
                can_fork: session.actions.contains(&Action::Fork),
                online,
            }
        })
        .collect();
    let shared_folders = node
        .shared_folders()
        .into_iter()
        .map(|folder| panel::SharedFolderItem {
            peer_key: folder.peer.to_string(),
            peer_name: node.peer_name(&folder.peer),
            online: peers_info.iter().any(|p| p.key == folder.peer && p.online),
            path: folder.path,
            can_write: folder.can_write,
        })
        .collect();
    panel::Data {
        node: card,
        peers,
        nearby,
        grants,
        resources,
        remote_sessions,
        shared_folders,
        local_version: status.app_version,
        now: secs(now_ms),
    }
}

/// What a panel event led to, for the app to finish (clipboard, center view).
#[derive(Clone, Debug)]
pub enum Outcome {
    Done,
    /// A notice for the user.
    Notice(String),
    KeyChecked { key: String, valid: bool },
    /// Text to put on the clipboard.
    Copy(String),
    /// A live view of a peer's session to show in the center.
    Opened(Arc<RemoteView>),
    /// A fork of a peer's session, now a local session with this id.
    Forked(String),
    /// A peer's shared folder, listed, to browse in the center.
    Folder { peer: String, folder: String, entries: Vec<crate::FolderEntry>, can_write: bool },
}

/// Carries out a panel event on `node`.
pub async fn apply(node: Node, event: panel::Event) -> Result<Outcome, String> {
    run(&node, event).await.map_err(|error| error.to_string())
}

async fn run(node: &Node, event: panel::Event) -> Result<Outcome, NodeError> {
    use panel::Event as E;
    Ok(match event {
        E::ValidateKey(key) => {
            let valid = Node::parse_key(&key).is_ok_and(|parsed| parsed != node.key());
            Outcome::KeyChecked { key, valid }
        }
        E::AddPeer { key, address } => {
            node.add_peer(&key, address.as_deref()).await?;
            Outcome::Done
        }
        E::AddDiscovered(key) => {
            node.add_nearby(&key).await?;
            Outcome::Done
        }
        E::CopyKey(key) => Outcome::Copy(key),
        E::RemovePeer(key) => {
            node.remove_peer(&key)?;
            Outcome::Done
        }
        E::RenamePeer { key, name } => {
            node.rename_peer(&key, &name)?;
            Outcome::Done
        }
        E::CreateGrant(input) => {
            let (scopes, until) = grant_request(&input, crate::now_ms())?;
            node.issue_grant(&input.peer_key, scopes, until, input.reshare).await?;
            Outcome::Notice("Grant created".to_owned())
        }
        E::RevokeGrant(id) => {
            node.revoke_grant(&GrantId(id)).await?;
            Outcome::Notice("Grant revoked".to_owned())
        }
        E::RenewGrant(id) => {
            let id = GrantId(id);
            let record = node
                .grant_store()
                .issued()?
                .into_iter()
                .find(|g| g.id == id)
                .ok_or_else(|| NodeError::Refused("unknown grant".into()))?;
            // Renewing gives the grant its original length again from now.
            let until = record.until_ms.map(|until| crate::now_ms() + (until - record.created_ms).max(60_000));
            node.renew_grant(&id, until).await?;
            Outcome::Notice("Grant renewed".to_owned())
        }
        E::Refresh => {
            node.refresh_presence().await;
            Outcome::Done
        }
        E::ScanTailscale => {
            let found = node.scan_tailscale().await?;
            Outcome::Notice(format!("Tailnet scan found {found} node{}", if found == 1 { "" } else { "s" }))
        }
        E::OpenRemote { peer, session } => Outcome::Opened(Arc::new(node.open_remote(&peer, &session).await?)),
        E::ForkRemote { peer, session } => Outcome::Forked(node.fork_remote(&peer, &session).await?),
        E::BrowseFolder { peer, folder } => {
            let entries = node.list_folder(&peer, &folder, "").await?;
            let can_write = node.shared_folders().iter().any(|f| f.peer.to_string() == peer && f.path == folder && f.can_write);
            Outcome::Folder { peer, folder, entries, can_write }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_ids_round_trip() {
        let resources = [
            Resource::Session { id: "s-1".into() },
            Resource::Prompt { session: "s:with:colons".into(), turn: DomNodeId(7) },
            Resource::Project { id: "/home/me/work".into() },
            Resource::Ssh { host: "box".into() },
            Resource::Surface { uri: "surface://browser/tab".into() },
            Resource::Folder { path: "/srv/my share".into() },
        ];
        for resource in resources {
            assert_eq!(parse_resource(&resource_id(&resource), 5), Some(resource));
        }
        assert_eq!(parse_resource("future", 42), Some(Resource::FutureSessions { after_ms: 42 }));
        assert_eq!(parse_resource("nonsense", 0), None);
    }

    #[test]
    fn panel_requests_become_scopes() {
        let input = |id: &str, scope, permission, future_sessions| panel::GrantInput {
            peer_key: "k".into(),
            resource: panel::GrantResource { id: id.into(), label: String::new(), scope, future_sessions },
            permission,
            ttl_seconds: 3600,
            reshare: false,
        };
        let (scopes, until) = grant_request(&input("prompt:3:s", panel::GrantScope::Prompt, panel::Permission::View, true), 1000).unwrap();
        assert_eq!(until, Some(1000 + 3_600_000));
        assert_eq!(scopes.len(), 2);
        assert_eq!(scopes[1].resource, Resource::FutureSessions { after_ms: 1000 });
        let (scopes, _) = grant_request(&input("ssh:box", panel::GrantScope::SshHost, panel::Permission::Drive, false), 0).unwrap();
        assert!(scopes[0].actions.contains(&Action::Exec));
        assert!(grant_request(&input("ssh:box", panel::GrantScope::SshHost, panel::Permission::Fork, false), 0).is_err());
        let (scopes, _) = grant_request(&input("session:s", panel::GrantScope::Session, panel::Permission::Drive, false), 0).unwrap();
        assert_eq!(permission(&scopes[0].actions), panel::Permission::Drive);
    }

    #[test]
    fn folder_requests_name_the_real_folder_view_reads_drive_writes() {
        let dir = tempfile::tempdir().unwrap();
        let real = loa_exec::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(real.join("share")).unwrap();
        std::fs::write(real.join("file.txt"), "x").unwrap();
        let input = |path: &str, permission| panel::GrantInput {
            peer_key: "k".into(),
            resource: panel::GrantResource { id: format!("folder:{path}"), label: String::new(), scope: panel::GrantScope::Folder, future_sessions: false },
            permission,
            ttl_seconds: 3600,
            reshare: false,
        };
        let share = real.join("share").to_str().unwrap().to_owned();
        // Typed with a trailing slash and a `..` detour: the grant names the real folder.
        let typed = format!("{}/../share/", share);
        let (scopes, _) = grant_request(&input(&typed, panel::Permission::View), 0).unwrap();
        assert_eq!(scopes[0].resource, Resource::Folder { path: share.clone() });
        assert_eq!(scopes[0].actions, [Action::Read].into());
        let (scopes, _) = grant_request(&input(&share, panel::Permission::Drive), 0).unwrap();
        assert_eq!(scopes[0].actions, [Action::Read, Action::Write].into());
        assert_eq!(permission(&scopes[0].actions), panel::Permission::Drive);
        for refused in ["share", "/no/such/folder", real.join("file.txt").to_str().unwrap()] {
            assert!(grant_request(&input(refused, panel::Permission::View), 0).is_err(), "{refused}");
        }
        assert!(grant_request(&input(&share, panel::Permission::Fork), 0).is_err());
    }
}
