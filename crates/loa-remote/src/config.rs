//! The Remote hosts of the app's central config ([`loa_config::RemoteHost`])
//! as connectable [`Host`]s, and `~/.ssh/config` aliases as new entries.
//!
//! The central config is the store: the Tools panel edits its
//! `remote_hosts`, and [`load`] rebuilds the [`crate::hosts::HostRegistry`]
//! from it.

use std::collections::BTreeMap;

use loa_config::RemoteHost;

use crate::error::Error;
use crate::hosts::{Host, HostSource};
use crate::limits::HostLimits;
use crate::ssh_config::{Overrides, SshConfig};

/// The limits a config entry sets. Empty `allowed_paths` keeps meaning no
/// file access; `auto_approve` covers both writes and commands.
pub fn limits(entry: &RemoteHost) -> HostLimits {
    HostLimits {
        allowed_paths: entry.allowed_paths.clone(),
        read_only: entry.read_only,
        commands: entry.commands,
        auto_approve_writes: entry.auto_approve,
        auto_approve_commands: entry.auto_approve,
        ..HostLimits::default()
    }
}

/// Resolves one entry through `ssh` the way `ssh -l USER -p PORT -J JUMP
/// HOST` would: `HOST` gets the config blocks matching it, the entry's own
/// values win.
pub fn host(name: &str, entry: &RemoteHost, ssh: &SshConfig, default_user: &str) -> Result<Host, Error> {
    let overrides = Overrides {
        user: entry.user.clone(),
        port: entry.port,
        proxy_jump: entry.proxy_jump.clone(),
    };
    let resolved = ssh.resolve_named(name, &entry.host, &overrides, default_user)?;
    let in_ssh_config = ssh.aliases().iter().any(|alias| alias.eq_ignore_ascii_case(&entry.host));
    let mut host = Host::from_config(resolved, limits(entry));
    if !in_ssh_config {
        host.source = HostSource::Manual;
    }
    Ok(host)
}

/// Every entry as a [`Host`], and the names that failed to resolve.
pub fn load(
    entries: &BTreeMap<String, RemoteHost>,
    ssh: &SshConfig,
    default_user: &str,
) -> (Vec<Host>, Vec<(String, Error)>) {
    let mut hosts = Vec::with_capacity(entries.len());
    let mut failed = Vec::new();
    for (name, entry) in entries {
        match host(name, entry, ssh, default_user) {
            Ok(host) => hosts.push(host),
            Err(e) => failed.push((name.clone(), e)),
        }
    }
    (hosts, failed)
}

/// A new entry for an alias of `~/.ssh/config`: the alias carries the
/// connection settings, the limits start closed (read only, no paths, no
/// commands) until the user opens them.
pub fn entry_for_alias(alias: &str) -> RemoteHost {
    RemoteHost {
        host: alias.to_string(),
        user: None,
        port: None,
        proxy_jump: None,
        allowed_paths: Vec::new(),
        read_only: true,
        commands: false,
        auto_approve: false,
    }
}

/// Adds every concrete alias of `ssh` that `entries` doesn't have yet.
/// Returns the names added; existing entries keep their limits.
pub fn import_aliases(entries: &mut BTreeMap<String, RemoteHost>, ssh: &SshConfig) -> Vec<String> {
    let mut added = Vec::new();
    for alias in ssh.aliases() {
        if !entries.contains_key(&alias) {
            entries.insert(alias.clone(), entry_for_alias(&alias));
            added.push(alias);
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn ssh(text: &str) -> SshConfig {
        SshConfig::parse(text, Path::new("/home/me/.ssh/config"), Path::new("/home/me")).unwrap()
    }

    #[test]
    fn aliases_resolve_through_ssh_config_and_manual_hosts_get_its_defaults() {
        let ssh = ssh("Host bastion\n HostName 10.0.0.1\n User jump\nHost db\n HostName db.internal\n ProxyJump bastion\nHost *\n User fallback\n");
        let mut entries = BTreeMap::new();
        assert_eq!(import_aliases(&mut entries, &ssh), ["bastion", "db"]);
        assert!(entries["db"].read_only && !entries["db"].commands && entries["db"].allowed_paths.is_empty());
        entries.insert(
            "box".into(),
            RemoteHost {
                host: "192.168.1.5".into(),
                user: Some("root".into()),
                port: Some(2222),
                proxy_jump: Some("bastion".into()),
                allowed_paths: vec!["/srv".into()],
                read_only: false,
                commands: true,
                auto_approve: true,
            },
        );
        entries.get_mut("db").unwrap().commands = true;
        assert!(import_aliases(&mut entries, &ssh).is_empty(), "nothing new");
        assert!(entries["db"].commands, "re-import keeps limits");

        let (hosts, failed) = load(&entries, &ssh, "me");
        assert!(failed.is_empty());
        let db = hosts.iter().find(|h| h.name == "db").unwrap();
        assert_eq!(db.target.hostname, "db.internal");
        assert_eq!(db.jumps[0].hostname, "10.0.0.1");
        assert_eq!(db.jumps[0].user, "jump");
        assert!(matches!(db.source, HostSource::SshConfig(_)));
        let b = hosts.iter().find(|h| h.name == "box").unwrap();
        assert_eq!((b.target.hostname.as_str(), b.target.port, b.target.user.as_str()), ("192.168.1.5", 2222, "root"));
        assert_eq!(b.jumps.len(), 1, "the entry's ProxyJump resolves through ssh config");
        assert_eq!(b.source, HostSource::Manual);
        assert!(b.limits.auto_approve_writes && b.limits.auto_approve_commands && b.limits.commands);
    }

    #[test]
    fn a_proxyjump_cycle_is_reported_per_host() {
        let ssh = ssh("Host a\n ProxyJump b\nHost b\n ProxyJump a\n");
        let mut entries = BTreeMap::new();
        import_aliases(&mut entries, &ssh);
        let (hosts, failed) = load(&entries, &ssh, "me");
        assert!(hosts.is_empty());
        assert_eq!(failed.len(), 2);
    }
}
