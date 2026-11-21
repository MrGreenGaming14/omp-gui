//! SSH connections over `russh`: ProxyJump chains, host key checks through [`KnownHosts`] and the
//! [`HostKeyDecider`] port, agent authentication, and one pooled connection per host.

use loa_util::LockExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client;
use russh::keys::PublicKeyOrCertificate;
use tokio::net::TcpStream;

use crate::agent;
use crate::error::{Error, Result};
use crate::hosts::{Endpoint, Host};
use crate::known_hosts::{HostKeyStatus, KnownHosts, fingerprint};
use crate::ports::{HostKeyChange, HostKeyDecider, HostKeyPrompt};
use crate::sftp::Sftp;

/// Connection settings the app passes in.
#[derive(Debug, Clone)]
pub struct ConnectorConfig {
    /// Agent socket for hops without `IdentityAgent` (normally `SSH_AUTH_SOCK`).
    pub default_agent: Option<PathBuf>,
    pub connect_timeout: Duration,
    /// Keepalive interval; a connection that misses three is redialled on next use.
    pub keepalive: Duration,
}

impl ConnectorConfig {
    /// Defaults with the agent socket taken from `SSH_AUTH_SOCK`.
    pub fn from_env() -> Self {
        Self::with_agent(default_agent_from_env())
    }

    pub fn with_agent(default_agent: Option<PathBuf>) -> Self {
        Self { default_agent, connect_timeout: Duration::from_secs(20), keepalive: Duration::from_secs(30) }
    }
}

/// `SSH_AUTH_SOCK`; on Windows, falling back to the OpenSSH agent pipe.
fn default_agent_from_env() -> Option<PathBuf> {
    let env = std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from);
    #[cfg(windows)]
    let env = env.or_else(|| Some(PathBuf::from(r"\\.\pipe\openssh-ssh-agent")));
    env
}

/// russh client handler: checks the server's host key.
pub(crate) struct ClientHandler {
    host: String,
    hop: Endpoint,
    known: Arc<KnownHosts>,
    decider: Arc<dyn HostKeyDecider>,
    rejection: Arc<Mutex<Option<String>>>,
}

impl ClientHandler {
    async fn check(&self, presented: &PublicKeyOrCertificate) -> std::result::Result<(), String> {
        let (hostname, port) = (&self.hop.hostname, self.hop.port);
        let change = match self.known.check(hostname, port, presented).map_err(|e| e.to_string())? {
            HostKeyStatus::Trusted => return Ok(()),
            HostKeyStatus::Revoked { entry: e } => return Err(format!("key {} is marked @revoked in {}:{}", e.fingerprint, e.file.display(), e.line)),
            HostKeyStatus::Unknown => HostKeyChange::New,
            HostKeyStatus::Changed { previous } => HostKeyChange::Changed { previous },
        };
        let (key_type, key) = match presented {
            PublicKeyOrCertificate::PublicKey { key, .. } => (key.algorithm().as_str().to_owned(), key.key_data()),
            PublicKeyOrCertificate::Certificate(c) => (c.algorithm().as_str().to_owned(), c.public_key()),
        };
        let changed = matches!(change, HostKeyChange::Changed { .. });
        let prompt = HostKeyPrompt { host: self.host.clone(), hostname: hostname.clone(), port, key_type, fingerprint: fingerprint(key), change };
        if !self.decider.decide(prompt).await {
            return Err(if changed { "the host key CHANGED and the new key was not accepted" } else { "the new host key was not accepted" }.into());
        }
        self.known.add(hostname, port, key).map_err(|e| format!("accepted, but saving it failed: {e}"))
    }
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, presented: &PublicKeyOrCertificate) -> std::result::Result<bool, Self::Error> {
        let checked = self.check(presented).await;
        let accepted = checked.is_ok();
        *self.rejection.locked() = checked.err();
        Ok(accepted)
    }
}

type SshHandle = client::Handle<ClientHandler>;

/// An authenticated connection, with the jump connections it tunnels through kept alive.
pub(crate) struct Session {
    pub(crate) host: String,
    pub(crate) handle: SshHandle,
    _jumps: Vec<SshHandle>,
    sftp: tokio::sync::Mutex<Option<Arc<Sftp>>>,
}

impl Session {
    /// The session's SFTP channel, opened on first use.
    pub(crate) async fn sftp(&self) -> Result<Arc<Sftp>> {
        let mut slot = self.sftp.lock().await;
        if let Some(s) = slot.as_ref() {
            return Ok(s.clone());
        }
        let s = Arc::new(Sftp::open(&self.handle, &self.host).await?);
        *slot = Some(s.clone());
        Ok(s)
    }

    /// Drops the SFTP channel so the next call opens a new one.
    pub(crate) async fn reset_sftp(&self) {
        *self.sftp.lock().await = None;
    }
}

type Slot = Arc<tokio::sync::Mutex<Option<(Vec<Endpoint>, Arc<Session>)>>>;

/// Dials hosts and keeps one connection per host for reuse.
pub struct Connector {
    known: Arc<KnownHosts>,
    decider: Arc<dyn HostKeyDecider>,
    config: ConnectorConfig,
    ssh: Arc<client::Config>,
    pool: Mutex<HashMap<String, Slot>>,
}

impl Connector {
    pub fn new(known: Arc<KnownHosts>, decider: Arc<dyn HostKeyDecider>, config: ConnectorConfig) -> Self {
        let ssh = client::Config { keepalive_interval: Some(config.keepalive), keepalive_max: 3, nodelay: true, ..client::Config::default() };
        Self { known, decider, config, ssh: Arc::new(ssh), pool: Mutex::default() }
    }

    /// Closes the pooled connection to `host`, if any.
    pub async fn disconnect(&self, host: &str) {
        let slot = self.pool.locked().remove(host);
        if let Some(slot) = slot
            && let Some((_, session)) = slot.lock().await.take()
        {
            let _ = session.handle.disconnect(russh::Disconnect::ByApplication, "", "en").await;
        }
    }

    /// A live connection to `host`: the pooled one unless it closed or the host's hops changed.
    pub(crate) async fn session(&self, host: &Host) -> Result<Arc<Session>> {
        let slot = self.pool.locked().entry(host.name.clone()).or_default().clone();
        let mut pooled = slot.lock().await;
        if let Some((hops, session)) = pooled.as_ref()
            && !session.handle.is_closed()
            && host.hops().eq(hops.iter())
        {
            return Ok(session.clone());
        }
        *pooled = None;
        let mut jumps: Vec<SshHandle> = Vec::with_capacity(host.jumps.len());
        for hop in &host.jumps {
            let next = self.hop(host, hop, jumps.last()).await?;
            jumps.push(next);
        }
        let handle = self.hop(host, &host.target, jumps.last()).await?;
        let session = Arc::new(Session { host: host.name.clone(), handle, _jumps: jumps, sftp: tokio::sync::Mutex::new(None) });
        *pooled = Some((host.hops().cloned().collect(), session.clone()));
        Ok(session)
    }

    /// Connects and authenticates one hop, directly or through `via`.
    async fn hop(&self, host: &Host, hop: &Endpoint, via: Option<&SshHandle>) -> Result<SshHandle> {
        let label = format!("{}:{}", hop.hostname, hop.port);
        let rejection = Arc::new(Mutex::new(None));
        let known = self.known.clone();
        let handler = ClientHandler { host: host.name.clone(), hop: hop.clone(), known, decider: self.decider.clone(), rejection: rejection.clone() };
        let ssh_err = |source| Error::Ssh { host: label.clone(), source };
        let connected = match via {
            None => {
                let tcp = tokio::time::timeout(self.config.connect_timeout, TcpStream::connect((hop.hostname.as_str(), hop.port)))
                    .await
                    .map_err(|_| Error::ConnectTimeout { host: label.clone() })?
                    .map_err(|source| Error::Connect { host: label.clone(), source })?;
                client::connect_stream(self.ssh.clone(), tcp, handler).await
            }
            Some(jump) => {
                let channel = jump.channel_open_direct_tcpip(hop.hostname.clone(), u32::from(hop.port), "127.0.0.1", 0).await.map_err(ssh_err)?;
                client::connect_stream(self.ssh.clone(), channel.into_stream(), handler).await
            }
        };
        let mut handle = connected.map_err(|source| match rejection.locked().take() {
            Some(reason) => Error::HostKeyRejected { host: label.clone(), reason },
            None => ssh_err(source),
        })?;
        let socket = agent::socket_for(&hop.agent, self.config.default_agent.as_deref(), &label)?;
        agent::authenticate(&mut handle, &hop.user, &label, &socket).await?;
        Ok(handle)
    }
}
