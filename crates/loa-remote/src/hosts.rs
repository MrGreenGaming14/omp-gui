//! The host list (#95): hosts imported from ssh config and hosts added by hand.

use std::collections::BTreeMap;
use std::sync::RwLock;

use crate::error::{Error, Result};
use crate::limits::HostLimits;
use crate::ssh_config::{
    AgentSocket, DEFAULT_PORT, Endpoint, Imported, Location, ResolvedHost, Unsupported,
};

/// Where a host entry came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostSource {
    SshConfig(Option<Location>),
    Manual,
}

/// A host the agent can reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// The name tools refer to it by.
    pub name: String,
    pub target: Endpoint,
    /// ProxyJump hops in connection order.
    pub jumps: Vec<Endpoint>,
    pub limits: HostLimits,
    pub source: HostSource,
    /// Config lines that may apply but are not acted on.
    pub unsupported: Vec<Unsupported>,
}

impl Host {
    /// A host added by hand, reached directly.
    pub fn manual(name: &str, hostname: &str, port: Option<u16>, user: &str) -> Self {
        Self {
            name: name.to_string(),
            target: Endpoint {
                alias: name.to_string(),
                hostname: hostname.to_string(),
                port: port.unwrap_or(DEFAULT_PORT),
                user: user.to_string(),
                agent: AgentSocket::Default,
            },
            jumps: Vec::new(),
            limits: HostLimits::default(),
            source: HostSource::Manual,
            unsupported: Vec::new(),
        }
    }

    /// A host from a resolved ssh config entry.
    pub fn from_config(resolved: ResolvedHost, limits: HostLimits) -> Self {
        Self {
            name: resolved.alias,
            target: resolved.target,
            jumps: resolved.jumps,
            limits,
            source: HostSource::SshConfig(resolved.location),
            unsupported: resolved.unsupported,
        }
    }

    /// Every hop, jumps first, the target last.
    pub fn hops(&self) -> impl Iterator<Item = &Endpoint> {
        self.jumps.iter().chain(std::iter::once(&self.target))
    }
}

/// The host list shown in the Remote hosts section.
#[derive(Debug, Default)]
pub struct HostRegistry {
    hosts: RwLock<BTreeMap<String, Host>>,
}

impl HostRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Host>> {
        // Writers only replace whole entries, so a poisoned map is consistent.
        self.hosts
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<String, Host>> {
        self.hosts
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Adds or replaces a host.
    pub fn insert(&self, host: Host) {
        self.write().insert(host.name.clone(), host);
    }

    pub fn remove(&self, name: &str) -> Option<Host> {
        self.write().remove(name)
    }

    pub fn get(&self, name: &str) -> Option<Host> {
        self.read().get(name).cloned()
    }

    pub fn list(&self) -> Vec<Host> {
        self.read().values().cloned().collect()
    }

    pub fn set_limits(&self, name: &str, limits: HostLimits) -> Result<()> {
        match self.write().get_mut(name) {
            Some(h) => {
                h.limits = limits;
                Ok(())
            }
            None => Err(Error::UnknownHost(name.to_string())),
        }
    }

    /// Merges an ssh config import. Hosts added by hand are left alone;
    /// re-imported hosts get the new connection data but keep their limits.
    /// Returns the aliases that failed to resolve.
    pub fn import(&self, imported: Vec<Imported>, limits: &HostLimits) -> Vec<(String, Error)> {
        let mut failed = Vec::new();
        let mut hosts = self.write();
        for Imported { alias, host } in imported {
            match host {
                Ok(resolved) => match hosts.get(&alias) {
                    Some(existing) if existing.source == HostSource::Manual => {}
                    Some(existing) => {
                        let kept = existing.limits.clone();
                        hosts.insert(alias, Host::from_config(resolved, kept));
                    }
                    None => {
                        hosts.insert(alias, Host::from_config(resolved, limits.clone()));
                    }
                },
                Err(e) => failed.push((alias, e)),
            }
        }
        failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_config::SshConfig;
    use std::path::Path;

    #[test]
    fn reimport_keeps_limits_and_manual_hosts_win() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        let parse = |text: &str| {
            let c = SshConfig::parse(text, &file, Path::new("/h")).unwrap();
            c.aliases()
                .into_iter()
                .map(|alias| Imported {
                    host: c.resolve(&alias, "me"),
                    alias,
                })
                .collect::<Vec<_>>()
        };
        let reg = HostRegistry::new();
        reg.insert(Host::manual("mine", "10.0.0.9", None, "root"));
        let failed = reg.import(
            parse("Host db\n HostName a\nHost mine\n HostName b\nHost loop\n ProxyJump loop\n"),
            &HostLimits::default(),
        );
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, "loop");
        reg.set_limits(
            "db",
            HostLimits {
                read_only: true,
                ..Default::default()
            },
        )
        .unwrap();
        reg.import(parse("Host db\n HostName moved\n"), &HostLimits::default());
        let db = reg.get("db").unwrap();
        assert_eq!(db.target.hostname, "moved");
        assert!(db.limits.read_only);
        assert_eq!(reg.get("mine").unwrap().target.hostname, "10.0.0.9");
    }
}
