//! The form issuing a grant, and how the panel names one; minting tokens stays with the app.

use loa_view::settings::peers::{Grant, GrantInput, GrantResource, GrantScope, Permission, ResourceChoice};

/// The resource and permission of a grant, either way it went.
pub fn grant_summary(grant: &Grant) -> String {
    let r = &grant.resource;
    let label = match r.scope {
        GrantScope::FutureSessions => "Every future session".to_owned(),
        GrantScope::Prompt if r.future_sessions => format!("{} and every session from now on", r.label),
        _ => r.label.clone(),
    };
    format!("{label}, {}", grant.permission)
}

/// The permission a newly picked resource starts with.
fn default_permission(scope: Option<GrantScope>) -> Permission {
    match scope {
        Some(GrantScope::Surface | GrantScope::SshHost | GrantScope::Folder) => Permission::View,
        Some(GrantScope::Sandbox) => Permission::Drive,
        _ => Permission::Fork,
    }
}

/// The grant being written; SSH access needs an explicit confirmation first.
#[derive(Debug, Clone)]
pub struct GrantForm {
    pub resource: Option<ResourceChoice>,
    pub permission: Permission,
    pub ttl_hours: String,
    pub reshare: bool,
    pub future_sessions: bool,
    pub ssh_confirmed: bool,
    /// A folder of this node typed in to grant; the app checks it exists.
    pub folder_path: String,
}

impl Default for GrantForm {
    fn default() -> Self { Self { resource: None, permission: Permission::Fork, ttl_hours: "24".into(), reshare: false, future_sessions: false, ssh_confirmed: false, folder_path: String::new() } }
}

/// Why a grant cannot be issued yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantFormError { MissingResource, InvalidTtl, InvalidPermission, SshNotConfirmed }

impl GrantForm {
    /// Another resource drops the SSH consent, the future sessions choice and the permission.
    pub fn select_resource(&mut self, resource: Option<ResourceChoice>) {
        if self.resource != resource {
            (self.ssh_confirmed, self.future_sessions) = (false, false);
            self.permission = default_permission(resource.as_ref().map(|r| r.scope));
        }
        self.resource = resource;
    }

    /// The typed folder as a resource; `None` while empty.
    pub fn folder_choice(&self) -> Option<ResourceChoice> {
        let path = self.folder_path.trim();
        (!path.is_empty()).then(|| ResourceChoice { id: format!("folder:{path}"), label: format!("Folder {path}"), scope: GrantScope::Folder })
    }

    /// Whether the resource reaches this node's SSH connection.
    pub fn is_ssh(&self) -> bool { self.resource.as_ref().is_some_and(|r| r.scope == GrantScope::SshHost) }

    /// The exact issuance request for `peer_key`, without minting anything.
    pub fn build(&self, peer_key: &str) -> Result<GrantInput, GrantFormError> {
        let r = self.resource.as_ref().ok_or(GrantFormError::MissingResource)?;
        if !self.permission.allowed_for(&r.scope) {
            return Err(GrantFormError::InvalidPermission);
        }
        if self.is_ssh() && !self.ssh_confirmed {
            return Err(GrantFormError::SshNotConfirmed);
        }
        let hours: u64 = self.ttl_hours.trim().parse().map_err(|_| GrantFormError::InvalidTtl)?;
        let ttl_seconds = hours.checked_mul(3600).filter(|s| *s > 0).ok_or(GrantFormError::InvalidTtl)?;
        let resource = GrantResource { id: r.id.clone(), label: r.label.clone(), scope: r.scope, future_sessions: r.scope == GrantScope::Prompt && self.future_sessions };
        Ok(GrantInput { peer_key: peer_key.to_owned(), resource, permission: self.permission, ttl_seconds, reshare: self.reshare })
    }
}
