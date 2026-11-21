//! Grants as the panel shows them, and the form issuing one; minting tokens stays with the app.

/// What a grant protects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantScope { Session, Prompt, Project, FutureSessions, SshHost, Surface, Sandbox, Folder }

/// A resource the app offers to grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceChoice {
    pub id: String,
    pub label: String,
    pub scope: GrantScope,
}

/// A granted resource; a prompt can also cover every future session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantResource {
    pub id: String,
    pub label: String,
    pub scope: GrantScope,
    pub future_sessions: bool,
}

loa_util::str_enum! {
    /// What the holder may do with the resource.
    #[derive(Default)]
    pub Permission { View => "view", Suggest => "suggest", Drive => "drive", #[default] Fork => "fork" }
}

impl Permission {
    /// Surfaces are viewed or driven (input); SSH hosts and folders read (view) or also
    /// written, and on SSH run (drive); a sandbox only runs tasks (drive).
    pub fn allowed_for(self, scope: &GrantScope) -> bool {
        match scope {
            GrantScope::Sandbox => self == Self::Drive,
            GrantScope::Surface | GrantScope::SshHost | GrantScope::Folder => matches!(self, Self::View | Self::Drive),
            _ => true,
        }
    }

    /// The permission a newly picked resource starts with.
    fn default_for(scope: Option<GrantScope>) -> Self {
        match scope {
            Some(GrantScope::Surface | GrantScope::SshHost | GrantScope::Folder) => Self::View,
            Some(GrantScope::Sandbox) => Self::Drive,
            _ => Self::Fork,
        }
    }
}

loa_util::str_enum! {
    /// Whether this node issued or received the grant.
    pub GrantDirection { Given => "Given", Received => "Received" }
}

/// A grant between this node and one peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub id: String,
    pub peer_key: String,
    pub direction: GrantDirection,
    pub resource: GrantResource,
    pub permission: Permission,
    /// When the grant ends; `None` renews until revoked.
    pub expires_at: Option<u64>,
    /// The next automatic renewal, if known.
    pub renews_at: Option<u64>,
    pub reshare: bool,
    /// Revoked by this node, or refused renewal by its issuer.
    pub ended: bool,
}

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

impl Grant {
    /// The expiry and the time to the next renewal against the app's clock `now` (seconds).
    pub fn expiry_text(&self, now: u64) -> String {
        if self.ended {
            return if self.direction == GrantDirection::Given { "revoked" } else { "revoked by its issuer" }.to_owned();
        }
        let expiry = match self.expires_at {
            None => "no end date".to_owned(),
            Some(at) if at <= now => "expired".to_owned(),
            Some(at) => format!("expires in {}", duration(at - now, true)),
        };
        match self.renews_at {
            Some(at) if at > now => format!("{expiry}; renews in {}", duration(at - now, true)),
            Some(_) => format!("{expiry}; renewal due"),
            None => expiry,
        }
    }
}

/// "45s", "5m", "3h", "2d": rounded `up` for countdowns, down for ages.
pub(crate) fn duration(seconds: u64, up: bool) -> String {
    let (unit, name) = match seconds {
        0..60 => return format!("{seconds}s"),
        60..3600 => (60, "m"),
        3600..86400 => (3600, "h"),
        _ => (86400, "d"),
    };
    format!("{}{name}", if up { seconds.div_ceil(unit) } else { seconds / unit })
}

/// A validated request for the app's grant issuer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantInput {
    pub peer_key: String,
    pub resource: GrantResource,
    pub permission: Permission,
    pub ttl_seconds: u64,
    pub reshare: bool,
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
            self.permission = Permission::default_for(resource.as_ref().map(|r| r.scope));
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
