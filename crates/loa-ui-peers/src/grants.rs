//! Grant display and issuance form; token creation stays with the app.

use std::fmt;

/// Resource category protected by a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantScope { Session, Prompt, Project, FutureSessions, SshHost, Surface }

/// A concrete resource in an app-supplied picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceChoice {
    pub id: String,
    pub label: String,
    pub scope: GrantScope,
}

/// Resources carried in a grant; a prompt can additionally include future sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantResource {
    pub id: String,
    pub label: String,
    pub scope: GrantScope,
    pub future_sessions: bool,
}

/// Capability a holder receives for this resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Permission { View, Suggest, Drive, #[default] Fork }
impl Permission {
    pub const ALL: [Self; 4] = [Self::View, Self::Suggest, Self::Drive, Self::Fork];
    /// Surface streams only allow viewing or sending input.
    pub fn allowed_for(self, scope: &GrantScope) -> bool {
        *scope != GrantScope::Surface || matches!(self, Self::View | Self::Drive)
    }
}
impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self { Self::View => "view", Self::Suggest => "suggest", Self::Drive => "drive", Self::Fork => "fork" })
    }
}

/// Whether this node issued or received the grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantDirection { Given, Received }
impl fmt::Display for GrantDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self { Self::Given => "Given", Self::Received => "Received" })
    }
}

/// App-supplied capability for a single peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub id: String,
    pub peer_key: String,
    pub direction: GrantDirection,
    pub resource: GrantResource,
    pub permission: Permission,
    pub expires_at: u64,
    /// Next planned automatic renewal, if known.
    pub renews_at: Option<u64>,
    pub reshare: bool,
}

/// Readable resource and permission summary shared by both grant directions.
pub fn grant_summary(grant: &Grant) -> String {
    let resource = &grant.resource;
    let label = if resource.scope == GrantScope::FutureSessions {
        "Every future session".to_owned()
    } else if resource.scope == GrantScope::Prompt && resource.future_sessions {
        format!("{} and every session from now on", resource.label)
    } else {
        resource.label.clone()
    };
    format!("{label}, {}", grant.permission)
}

impl Grant {
    /// Display expiry and time until the next renewal using the app's clock.
    pub fn expiry_text(&self, now: u64) -> String {
        let expiry = if self.expires_at <= now { "expired".to_owned() } else {
            format!("expires in {}", duration(self.expires_at - now))
        };
        match self.renews_at {
            Some(renewal) if renewal > now => format!("{expiry}; renews in {}", duration(renewal - now)),
            Some(_) => format!("{expiry}; renewal due"),
            None => expiry,
        }
    }
}
fn duration(seconds: u64) -> String {
    if seconds < 60 { format!("{seconds}s") }
    else if seconds < 3600 { format!("{}m", seconds.div_ceil(60)) }
    else if seconds < 86400 { format!("{}h", seconds.div_ceil(3600)) }
    else { format!("{}d", seconds.div_ceil(86400)) }
}

/// Validated request for the app's grant issuer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantInput {
    pub peer_key: String,
    pub resource: GrantResource,
    pub permission: Permission,
    pub ttl_seconds: u64,
    pub reshare: bool,
}

/// User-entered grant fields; no SSH capability exists before explicit confirmation.
#[derive(Debug, Clone)]
pub struct GrantForm {
    pub resource: Option<ResourceChoice>,
    pub permission: Permission,
    pub ttl_hours: String,
    pub reshare: bool,
    pub future_sessions: bool,
    pub ssh_confirmed: bool,
    pub ssh_confirmation_pending: bool,
}
impl Default for GrantForm {
    fn default() -> Self {
        Self { resource: None, permission: Permission::Fork, ttl_hours: "24".into(), reshare: false, future_sessions: false, ssh_confirmed: false, ssh_confirmation_pending: false }
    }
}

/// Why a grant cannot yet be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrantFormError {
    #[error("select a resource")]
    MissingResource,
    #[error("TTL must be a positive whole number of hours")]
    InvalidTtl,
    #[error("permission is not available for this resource")]
    InvalidPermission,
    #[error("SSH access requires explicit confirmation")]
    SshNotConfirmed,
}
impl GrantForm {
    /// Selecting another resource invalidates prior SSH consent and unsupported permissions.
    pub fn select_resource(&mut self, resource: Option<ResourceChoice>) {
        if self.resource != resource {
            self.ssh_confirmed = false;
            self.ssh_confirmation_pending = false;
            self.future_sessions = false;
            self.permission = if resource.as_ref().is_some_and(|r| r.scope == GrantScope::Surface) { Permission::View } else { Permission::Fork };
        }
        self.resource = resource;
    }

    /// Whether the selected resource gives access to this node's SSH connection.
    pub fn is_ssh(&self) -> bool {
        self.resource.as_ref().is_some_and(|r| r.scope == GrantScope::SshHost)
    }

    /// Validate and build the exact issuance request, without minting a token.
    pub fn build(&self, peer_key: &str) -> Result<GrantInput, GrantFormError> {
        let resource = self.resource.as_ref().ok_or(GrantFormError::MissingResource)?;
        if !self.permission.allowed_for(&resource.scope) { return Err(GrantFormError::InvalidPermission); }
        if self.is_ssh() && !self.ssh_confirmed { return Err(GrantFormError::SshNotConfirmed); }
        let hours: u64 = self.ttl_hours.trim().parse().map_err(|_| GrantFormError::InvalidTtl)?;
        let ttl_seconds = hours.checked_mul(3600).filter(|value| *value > 0).ok_or(GrantFormError::InvalidTtl)?;
        Ok(GrantInput {
            peer_key: peer_key.to_owned(),
            resource: GrantResource { id: resource.id.clone(), label: resource.label.clone(), scope: resource.scope, future_sessions: resource.scope == GrantScope::Prompt && self.future_sessions },
            permission: self.permission,
            ttl_seconds,
            reshare: self.reshare,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Data, Event, Message, State};
    use crate::peers::{Discovery, Peer, Presence, Transport};

    fn choice(scope: GrantScope) -> ResourceChoice {
        ResourceChoice { id: "resource".into(), label: "Prompt A".into(), scope }
    }
    fn grant(scope: GrantScope) -> Grant {
        Grant { id: "grant".into(), peer_key: "peer".into(), direction: GrantDirection::Given,
            resource: GrantResource { id: "resource".into(), label: "Prompt A".into(), scope, future_sessions: true },
            permission: Permission::View, expires_at: 7200, renews_at: Some(3600), reshare: false }
    }
    fn state() -> State {
        let mut state = State::default();
        state.set_data(Data { peers: vec![Peer { key: "peer".into(), name: "Alice".into(), discovery: Discovery::Lan, presence: Presence::Online, last_seen: None, transport: Transport::Direct, app_version: "1.0.0".into(), shared_protocol: true }], grants: vec![grant(GrantScope::Prompt)], resources: vec![choice(GrantScope::Prompt), ResourceChoice { id: "ssh".into(), label: "Host".into(), scope: GrantScope::SshHost }], ..Data::default() });
        state
    }

    #[test]
    fn fork_is_default_but_surface_restricts_to_view_or_drive() {
        let mut form = GrantForm::default();
        assert_eq!(form.permission, Permission::Fork);
        assert!(!form.reshare);
        form.select_resource(Some(choice(GrantScope::Surface)));
        assert_eq!(form.permission, Permission::View);
        form.permission = Permission::Fork;
        assert_eq!(form.build("peer"), Err(GrantFormError::InvalidPermission));
        form.permission = Permission::Drive;
        assert!(form.build("peer").is_ok());
    }

    #[test]
    fn ttl_and_explicit_ssh_confirmation_guard_issuance() {
        let mut state = state();
        let _ = state.update(Message::SelectPeer("peer".into()));
        let _ = state.update(Message::SelectResource("ssh".into()));
        assert_eq!(state.grant_form.build("peer"), Err(GrantFormError::SshNotConfirmed));
        assert!(state.update(Message::SubmitGrant).1.is_none());
        let _ = state.update(Message::RequestSshConfirmation);
        let _ = state.update(Message::ConfirmSsh);
        assert!(matches!(state.update(Message::SubmitGrant).1, Some(Event::CreateGrant(_))));
        let _ = state.update(Message::TtlChanged("0".into()));
        assert_eq!(state.grant_form.build("peer"), Err(GrantFormError::InvalidTtl));
        let _ = state.update(Message::SelectResource("resource".into()));
        let _ = state.update(Message::SelectResource("ssh".into()));
        assert_eq!(state.grant_form.build("peer"), Err(GrantFormError::SshNotConfirmed));
    }

    #[test]
    fn prompt_future_sessions_and_permission_are_emitted_to_issuer() {
        let mut state = state();
        let _ = state.update(Message::SelectPeer("peer".into()));
        let _ = state.update(Message::SelectResource("resource".into()));
        let _ = state.update(Message::FutureSessionsChanged(true));
        let _ = state.update(Message::SelectPermission(Permission::View));
        let Some(Event::CreateGrant(request)) = state.update(Message::SubmitGrant).1 else {
            panic!("valid grant must reach issuer");
        };
        assert_eq!(request.resource.scope, GrantScope::Prompt);
        assert!(request.resource.future_sessions);
        assert_eq!(request.permission, Permission::View);
        assert_eq!(request.ttl_seconds, 24 * 3600);
    }

    #[test]
    fn remove_peer_requires_confirmation_and_rename_rejects_empty_name() {
        let mut state = state();
        let _ = state.update(Message::BeginRemove("peer".into()));
        let _ = state.update(Message::CancelRemove);
        assert!(state.update(Message::ConfirmRemove).1.is_none());
        let _ = state.update(Message::BeginRemove("peer".into()));
        assert_eq!(state.update(Message::ConfirmRemove).1, Some(Event::RemovePeer("peer".into())));
        let _ = state.update(Message::BeginRename("peer".into()));
        let _ = state.update(Message::RenameChanged("  ".into()));
        assert!(state.update(Message::SaveRename).1.is_none());
        let _ = state.update(Message::BeginRename("peer".into()));
        let _ = state.update(Message::RenameChanged(" New name ".into()));
        assert_eq!(state.update(Message::SaveRename).1, Some(Event::RenamePeer { key: "peer".into(), name: "New name".into() }));
    }

    #[test]
    fn summary_includes_future_sessions_and_expiry_renewal() {
        let grant = grant(GrantScope::Prompt);
        assert_eq!(grant_summary(&grant), "Prompt A and every session from now on, view");
        assert_eq!(grant.expiry_text(0), "expires in 2h; renews in 1h");
        assert_eq!(grant.expiry_text(7300), "expired; renewal due");
    }

    #[test]
    fn revoke_requires_confirmation_and_only_given_grants_can_be_revoked() {
        let mut state = state();
        let _ = state.update(Message::BeginRevoke("grant".into()));
        assert!(state.update(Message::CancelRevoke).1.is_none());
        assert!(state.update(Message::ConfirmRevoke).1.is_none());
        let _ = state.update(Message::BeginRevoke("grant".into()));
        assert_eq!(state.update(Message::ConfirmRevoke).1, Some(Event::RevokeGrant("grant".into())));
        state.data.grants[0].direction = GrantDirection::Received;
        let _ = state.update(Message::BeginRevoke("grant".into()));
        assert!(state.update(Message::ConfirmRevoke).1.is_none());
    }

    #[test]
    fn add_peer_waits_for_app_validation_of_current_key() {
        let mut state = state();
        assert_eq!(state.update(Message::KeyChanged("key-one".into())).1, Some(Event::ValidateKey("key-one".into())));
        state.set_key_validation("key-one", true);
        let _ = state.update(Message::KeyChanged("key-two".into()));
        state.set_key_validation("key-one", true);
        assert!(state.update(Message::AddPeer).1.is_none());
        state.set_key_validation("key-two", true);
        let _ = state.update(Message::AddressChanged(" 127.0.0.1:31415 ".into()));
        assert_eq!(state.update(Message::AddPeer).1, Some(Event::AddPeer { key: "key-two".into(), address: Some("127.0.0.1:31415".into()) }));
    }

    #[test]
    fn nearby_nodes_obey_filter_and_sort_and_cannot_be_added_twice() {
        let mut state = state();
        state.data.nearby = vec![
            Peer { key: "near-z".into(), name: "Zara".into(), discovery: Discovery::Lan, presence: Presence::Offline, last_seen: Some(1), transport: Transport::None, app_version: "1.0.0".into(), shared_protocol: true },
            Peer { key: "near-a".into(), name: "Alice nearby".into(), discovery: Discovery::Tailscale, presence: Presence::Online, last_seen: Some(2), transport: Transport::Direct, app_version: "1.0.0".into(), shared_protocol: true },
            state.data.peers[0].clone(),
        ];
        state.sort = crate::PeerSort::OnlineFirst;
        assert_eq!(state.visible_nearby().iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), ["near-a", "near-z"]);
        state.filter = "zara".into();
        assert_eq!(state.visible_nearby().iter().map(|p| p.key.as_str()).collect::<Vec<_>>(), ["near-z"]);
        assert!(state.update(Message::AddDiscovered("peer".into())).1.is_none());
    }

    #[test]
    fn replaced_snapshot_cannot_revoke_removed_grant_or_issue_removed_resource() {
        let mut state = state();
        let _ = state.update(Message::SelectPeer("peer".into()));
        let _ = state.update(Message::SelectResource("resource".into()));
        let _ = state.update(Message::BeginRevoke("grant".into()));
        let mut snapshot = state.data.clone();
        snapshot.grants.clear();
        snapshot.resources.clear();
        state.set_data(snapshot);
        assert!(state.update(Message::ConfirmRevoke).1.is_none());
        assert!(state.update(Message::SubmitGrant).1.is_none());
    }
}
