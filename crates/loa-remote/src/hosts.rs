//! The hosts the agent can reach, as the app's config resolved them.

use loa_util::RwLockExt;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::RwLock;

use crate::limits::HostLimits;

/// Which `ssh-agent` socket authenticates a hop.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AgentSocket {
    /// The socket the app was given (normally `SSH_AUTH_SOCK`).
    #[default]
    Default,
    Path(PathBuf),
    /// `IdentityAgent none`: no way to authenticate.
    Disabled,
}

/// One SSH hop: where to connect and as whom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub alias: String,
    pub hostname: String,
    pub port: u16,
    pub user: String,
    pub agent: AgentSocket,
}

/// A host the agent can reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub name: String,
    pub target: Endpoint,
    /// ProxyJump hops in connection order.
    pub jumps: Vec<Endpoint>,
    pub limits: HostLimits,
}

impl Host {
    /// Every hop, jumps first, the target last.
    pub fn hops(&self) -> impl Iterator<Item = &Endpoint> {
        self.jumps.iter().chain(std::iter::once(&self.target))
    }
}

/// The resolved Remote hosts, by name.
#[derive(Debug, Default)]
pub struct HostRegistry {
    hosts: RwLock<BTreeMap<String, Host>>,
}

impl HostRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, host: Host) {
        self.hosts.write_ok().insert(host.name.clone(), host);
    }

    /// Replaces the whole list, e.g. after the config changed.
    pub fn replace_all(&self, hosts: impl IntoIterator<Item = Host>) {
        *self.hosts.write_ok() = hosts.into_iter().map(|h| (h.name.clone(), h)).collect();
    }

    pub fn get(&self, name: &str) -> Option<Host> {
        self.hosts.read_ok().get(name).cloned()
    }
}
