//! The Remote hosts of the app's central config ([`RemoteHost`]) as connectable [`Host`]s, resolved
//! through an [`SshResolver`] one hop at a time, and `~/.ssh/config` aliases as new entries.

use std::collections::BTreeMap;
use std::path::PathBuf;

use loa_config::RemoteHost;

use crate::error::{Error, Result};
use crate::hosts::{AgentSocket, Endpoint, Host};
use crate::limits::HostLimits;
use crate::resolve::{ResolvedHost, SshResolver};

/// The longest ProxyJump chain followed.
pub const MAX_JUMPS: usize = 16;

/// The limits a config entry sets; `auto_approve` covers writes and commands.
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

/// Resolves one entry like `ssh -l USER -p PORT -J JUMP HOST`: the entry's own values win.
pub fn host(name: &str, entry: &RemoteHost, resolver: &dyn SshResolver) -> Result<Host> {
    let jump = entry.proxy_jump.as_deref();
    let (target, jumps) = chain(resolver, &entry.host, entry.user.as_deref(), entry.port, jump, &mut Vec::new())?;
    Ok(Host { name: name.to_owned(), target, jumps, limits: limits(entry) })
}

/// Every entry as a [`Host`], and the names that failed to resolve.
pub fn load(entries: &BTreeMap<String, RemoteHost>, resolver: &dyn SshResolver) -> (Vec<Host>, Vec<(String, Error)>) {
    let (mut hosts, mut failed) = (Vec::new(), Vec::new());
    for (name, entry) in entries {
        match host(name, entry, resolver) {
            Ok(host) => hosts.push(host),
            Err(e) => failed.push((name.clone(), e)),
        }
    }
    (hosts, failed)
}

/// A new entry for an ssh config alias; limits start closed (read only, no paths, no commands).
pub fn entry_for_alias(alias: &str) -> RemoteHost {
    RemoteHost {
        host: alias.to_owned(),
        user: None,
        port: None,
        proxy_jump: None,
        allowed_paths: Vec::new(),
        read_only: true,
        commands: false,
        auto_approve: false,
    }
}

/// Adds every alias `entries` doesn't have yet; returns the names added.
pub fn import_aliases(entries: &mut BTreeMap<String, RemoteHost>, resolver: &dyn SshResolver) -> Vec<String> {
    let added: Vec<String> = resolver.aliases().into_iter().filter(|alias| !entries.contains_key(alias)).collect();
    entries.extend(added.iter().map(|alias| (alias.clone(), entry_for_alias(alias))));
    added
}

/// Resolves `host` and its ProxyJump list: the first jump recursively, the others as given.
fn chain(
    resolver: &dyn SshResolver,
    host: &str,
    user: Option<&str>,
    port: Option<u16>,
    jump: Option<&str>,
    stack: &mut Vec<String>,
) -> Result<(Endpoint, Vec<Endpoint>)> {
    let key = host.to_ascii_lowercase();
    if stack.contains(&key) {
        return Err(Error::ProxyJumpCycle(stack[0].clone()));
    }
    if stack.len() > MAX_JUMPS {
        return Err(Error::ProxyJumpTooDeep(stack[0].clone()));
    }
    let resolved = resolver.lookup(host, user, port)?;
    let list = jump.map(str::to_owned).or_else(|| resolved.proxy_jump.clone()).filter(|l| !l.eq_ignore_ascii_case("none"));
    let mut jumps = Vec::new();
    for (i, spec) in list.iter().flat_map(|l| l.split(',')).enumerate() {
        let (h, u, p) = parse_jump(spec.trim()).map_err(|message| Error::Resolve { host: host.to_owned(), message })?;
        if i == 0 {
            stack.push(key.clone());
            let (first, before) = chain(resolver, &h, u.as_deref(), p, None, stack)?;
            stack.pop();
            jumps.extend(before);
            jumps.push(first);
        } else {
            jumps.push(endpoint(&h, resolver.lookup(&h, u.as_deref(), p)?));
        }
    }
    if jumps.len() > MAX_JUMPS {
        return Err(Error::ProxyJumpTooDeep(host.to_owned()));
    }
    Ok((endpoint(host, resolved), jumps))
}

fn endpoint(alias: &str, r: ResolvedHost) -> Endpoint {
    let agent = match r.identity_agent.as_deref() {
        None | Some("SSH_AUTH_SOCK" | "$SSH_AUTH_SOCK") => AgentSocket::Default,
        Some(v) if v.eq_ignore_ascii_case("none") => AgentSocket::Disabled,
        Some(v) => AgentSocket::Path(match v.strip_prefix("~/") {
            Some(rest) => loa_util::fs::home().unwrap_or_default().join(rest),
            None => PathBuf::from(v),
        }),
    };
    Endpoint { alias: alias.to_owned(), hostname: r.hostname, port: r.port, user: r.user, agent }
}

/// `[ssh://][user@]host[:port]`, IPv6 hosts in brackets.
fn parse_jump(spec: &str) -> std::result::Result<(String, Option<String>, Option<u16>), String> {
    let bad = |what: &str| format!("ProxyJump `{spec}`: {what}");
    let s = spec.strip_prefix("ssh://").unwrap_or(spec);
    let (user, hostport) = s.rsplit_once('@').map_or((None, s), |(u, h)| (Some(u.to_owned()), h));
    let (host, port) = match hostport.strip_prefix('[') {
        Some(rest) => match rest.split_once(']').ok_or_else(|| bad("missing `]`"))? {
            (host, "") => (host, None),
            (host, after) => (host, Some(after.strip_prefix(':').ok_or_else(|| bad("junk after `]`"))?)),
        },
        None => match hostport.split_once(':') {
            Some((h, p)) if !p.contains(':') => (h, Some(p)),
            _ => (hostport, None),
        },
    };
    if host.is_empty() {
        return Err(bad("empty host"));
    }
    let port = port.map(|p| p.parse::<u16>().ok().filter(|&p| p != 0).ok_or_else(|| bad("bad port"))).transpose()?;
    Ok((host.to_owned(), user, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::Static;

    #[test]
    fn entries_resolve_hop_by_hop_and_import_keeps_limits() {
        let ssh = Static::new("me").with("bastion", "10.0.0.1", 22, None).with("db", "db.internal", 22, Some("bastion,jump@[::1]:2200"));
        let ssh = ssh.with("a", "a", 22, Some("b")).with("b", "b", 22, Some("a"));
        let mut entries = BTreeMap::new();
        assert_eq!(import_aliases(&mut entries, &ssh), ["a", "b", "bastion", "db"]);
        assert!(entries["db"].read_only && !entries["db"].commands && entries["db"].allowed_paths.is_empty());
        entries.get_mut("db").unwrap().commands = true;
        let manual = RemoteHost { host: "192.168.1.5".into(), user: Some("root".into()), port: Some(2222), proxy_jump: Some("bastion".into()), ..entry_for_alias("") };
        entries.insert("box".into(), RemoteHost { auto_approve: true, ..manual });
        assert!(import_aliases(&mut entries, &ssh).is_empty() && entries["db"].commands, "re-import adds nothing, keeps limits");

        let (hosts, failed) = load(&entries, &ssh);
        assert_eq!(failed.iter().map(|(n, e)| (n.as_str(), matches!(e, Error::ProxyJumpCycle(_)))).collect::<Vec<_>>(), [("a", true), ("b", true)]);
        let db = hosts.iter().find(|h| h.name == "db").unwrap();
        let hops: Vec<_> = db.hops().map(|e| (e.hostname.as_str(), e.port, e.user.as_str())).collect();
        assert_eq!(hops, [("10.0.0.1", 22, "me"), ("::1", 2200, "jump"), ("db.internal", 22, "me")]);
        let b = hosts.iter().find(|h| h.name == "box").unwrap();
        assert_eq!((b.target.hostname.as_str(), b.target.port, b.target.user.as_str(), b.jumps.len()), ("192.168.1.5", 2222, "root", 1));
        assert!(b.limits.auto_approve_writes && b.limits.auto_approve_commands);
        for bad in ["", "h:0", "[::1", "[::1]x"] {
            assert!(parse_jump(bad).is_err(), "{bad}");
        }
    }
}
