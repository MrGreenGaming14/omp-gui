//! The [`SshResolver`] port: how a host name becomes hostname, user, port, ProxyJump and agent,
//! with the OpenSSH client ([`OpenSshCli`], `ssh -G`) and a fixed table ([`Static`]) as adapters.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use loa_util::ssh::ResolvedHost;

use crate::error::{Error, Result};

/// Resolves one hop the way `ssh [-l user] [-p port] host` would, and lists the config's aliases.
pub trait SshResolver: Send + Sync {
    fn aliases(&self) -> Vec<String>;
    fn lookup(&self, host: &str, user: Option<&str>, port: Option<u16>) -> Result<ResolvedHost>;
}

/// `ssh -G -F <config>`: the user's OpenSSH client evaluates the config, `Match` blocks included.
#[derive(Clone, Debug)]
pub struct OpenSshCli {
    pub program: PathBuf,
    pub config: PathBuf,
}

impl OpenSshCli {
    pub fn new(config: PathBuf) -> Self {
        Self { program: PathBuf::from("ssh"), config }
    }
}

impl SshResolver for OpenSshCli {
    fn aliases(&self) -> Vec<String> {
        loa_util::ssh::aliases(&self.config)
    }

    fn lookup(&self, host: &str, user: Option<&str>, port: Option<u16>) -> Result<ResolvedHost> {
        let config = if self.config.is_file() { self.config.display().to_string() } else { "none".to_owned() };
        let mut args = vec!["-F".to_owned(), config];
        args.extend(user.into_iter().flat_map(|u| ["-l".to_owned(), u.to_owned()]));
        args.extend(port.into_iter().flat_map(|p| ["-p".to_owned(), p.to_string()]));
        loa_util::ssh::resolve_with(&self.program, &args, host).map_err(|e| Error::Resolve { host: host.to_owned(), message: e.to_string() })
    }
}

/// A fixed table; unknown hosts resolve to themselves on port 22 as `user`.
#[derive(Clone, Debug, Default)]
pub struct Static {
    pub user: String,
    pub hosts: BTreeMap<String, ResolvedHost>,
}

impl Static {
    pub fn new(user: &str) -> Self {
        Self { user: user.to_owned(), hosts: BTreeMap::new() }
    }

    pub fn with(mut self, alias: &str, hostname: &str, port: u16, proxy_jump: Option<&str>) -> Self {
        let host = ResolvedHost { hostname: hostname.into(), user: self.user.clone(), port, proxy_jump: proxy_jump.map(Into::into), identity_agent: None };
        self.hosts.insert(alias.to_owned(), host);
        self
    }
}

impl SshResolver for Static {
    fn aliases(&self) -> Vec<String> {
        self.hosts.keys().cloned().collect()
    }

    fn lookup(&self, host: &str, user: Option<&str>, port: Option<u16>) -> Result<ResolvedHost> {
        let mut resolved = self.hosts.get(host).cloned().unwrap_or_else(|| ResolvedHost {
            hostname: host.to_owned(),
            user: self.user.clone(),
            port: 22,
            ..ResolvedHost::default()
        });
        resolved.user = user.map_or(resolved.user, str::to_owned);
        resolved.port = port.unwrap_or(resolved.port);
        Ok(resolved)
    }
}
