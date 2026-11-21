//! SSH connections over `russh`: ProxyJump chains, host key checks through
//! [`KnownHosts`] and the [`HostKeyDecider`] port, agent authentication, and
//! one reused connection per host.

use loa_util::LockExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::client;
use russh::keys::PublicKeyOrCertificate;
use tokio::net::TcpStream;

use crate::agent;
use crate::error::{Error, Result};
use crate::hosts::Host;
use crate::known_hosts::{HostKeyStatus, KnownHosts, fingerprint};
use crate::ports::{HostKeyChange, HostKeyDecider, HostKeyPrompt};
use crate::sftp::Sftp;
use crate::ssh_config::Endpoint;

/// Connection settings the app passes in.
#[derive(Debug, Clone)]
pub struct ConnectorConfig {
    /// Agent socket for hops without `IdentityAgent` (normally `SSH_AUTH_SOCK`).
    pub default_agent: Option<PathBuf>,
    /// Limit on opening the TCP connection to the first hop.
    pub connect_timeout: Duration,
    /// Keepalive interval; a connection that misses three is dropped and
    /// redialled on next use.
    pub keepalive: Duration,
}

impl ConnectorConfig {
    /// Defaults with the agent socket taken from `SSH_AUTH_SOCK`.
    pub fn from_env() -> Self {
        Self {
            default_agent: std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
            ..Self::with_agent(None)
        }
    }

    /// Defaults with an explicit agent socket.
    pub fn with_agent(default_agent: Option<PathBuf>) -> Self {
        Self {
            default_agent,
            connect_timeout: Duration::from_secs(20),
            keepalive: Duration::from_secs(30),
        }
    }
}

/// russh client handler: checks the server's host key.
pub(crate) struct ClientHandler {
    host: String,
    hostname: String,
    port: u16,
    known: Arc<KnownHosts>,
    decider: Arc<dyn HostKeyDecider>,
    rejection: Arc<std::sync::Mutex<Option<String>>>,
}

impl ClientHandler {
    fn reject(&self, reason: String) {
        *self
            .rejection.locked() = Some(reason);
    }
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        presented: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Self::Error> {
        let status = match self.known.check(&self.hostname, self.port, presented) {
            Ok(s) => s,
            Err(e) => {
                self.reject(e.to_string());
                return Ok(false);
            }
        };
        let change = match status {
            HostKeyStatus::Trusted => return Ok(true),
            HostKeyStatus::Revoked { entry } => {
                self.reject(format!(
                    "key {} is marked @revoked in {}:{}",
                    entry.fingerprint,
                    entry.file.display(),
                    entry.line
                ));
                return Ok(false);
            }
            HostKeyStatus::Unknown => HostKeyChange::New,
            HostKeyStatus::Changed { previous } => HostKeyChange::Changed { previous },
        };
        let (key_type, key) = match presented {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                (key.algorithm().as_str().to_string(), key.key_data())
            }
            PublicKeyOrCertificate::Certificate(c) => {
                (c.algorithm().as_str().to_string(), c.public_key())
            }
        };
        let changed = matches!(change, HostKeyChange::Changed { .. });
        let prompt = HostKeyPrompt {
            host: self.host.clone(),
            hostname: self.hostname.clone(),
            port: self.port,
            key_type,
            fingerprint: fingerprint(key),
            change,
        };
        if !self.decider.decide(prompt).await {
            self.reject(if changed {
                "the host key CHANGED and the new key was not accepted".to_string()
            } else {
                "the new host key was not accepted".to_string()
            });
            return Ok(false);
        }
        if let Err(e) = self.known.add(&self.hostname, self.port, key) {
            self.reject(format!("accepted, but saving it failed: {e}"));
            return Ok(false);
        }
        Ok(true)
    }
}

type SshHandle = client::Handle<ClientHandler>;

/// An authenticated connection to a host, with the jump connections it
/// tunnels through kept alive alongside it.
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

struct Pooled {
    hops: Vec<Endpoint>,
    session: Arc<Session>,
}

type Slot = Arc<tokio::sync::Mutex<Option<Pooled>>>;

/// Dials hosts and keeps one connection per host for reuse.
pub struct Connector {
    known: Arc<KnownHosts>,
    decider: Arc<dyn HostKeyDecider>,
    config: ConnectorConfig,
    ssh: Arc<client::Config>,
    pool: std::sync::Mutex<HashMap<String, Slot>>,
}

impl Connector {
    pub fn new(
        known: Arc<KnownHosts>,
        decider: Arc<dyn HostKeyDecider>,
        config: ConnectorConfig,
    ) -> Self {
        let ssh = client::Config {
            keepalive_interval: Some(config.keepalive),
            keepalive_max: 3,
            nodelay: true,
            ..client::Config::default()
        };
        Self {
            known,
            decider,
            config,
            ssh: Arc::new(ssh),
            pool: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Closes the pooled connection to `host`, if any.
    pub async fn disconnect(&self, host: &str) {
        let slot = self.pool.locked().remove(host);
        if let Some(slot) = slot
            && let Some(p) = slot.lock().await.take()
        {
            let _ = p
                .session
                .handle
                .disconnect(russh::Disconnect::ByApplication, "", "en")
                .await;
        }
    }

    /// A live connection to `host`: the pooled one, or a new one when there
    /// is none, it closed, or the host's hops changed.
    pub(crate) async fn session(&self, host: &Host) -> Result<Arc<Session>> {
        let slot = self.pool.locked().entry(host.name.clone()).or_default().clone();
        let mut pooled = slot.lock().await;
        if let Some(p) = pooled.as_ref()
            && !p.session.handle.is_closed()
            && host.hops().eq(p.hops.iter())
        {
            return Ok(p.session.clone());
        }
        *pooled = None;
        let session = Arc::new(self.dial(host).await?);
        *pooled = Some(Pooled {
            hops: host.hops().cloned().collect(),
            session: session.clone(),
        });
        Ok(session)
    }

    async fn dial(&self, host: &Host) -> Result<Session> {
        let mut jumps: Vec<SshHandle> = Vec::with_capacity(host.jumps.len());
        let mut via: Option<SshHandle> = None;
        for hop in &host.jumps {
            let next = self.hop(host, hop, via.as_ref()).await?;
            jumps.extend(via.replace(next));
        }
        let handle = self.hop(host, &host.target, via.as_ref()).await?;
        jumps.extend(via);
        Ok(Session {
            host: host.name.clone(),
            handle,
            _jumps: jumps,
            sftp: tokio::sync::Mutex::new(None),
        })
    }

    /// Connects and authenticates one hop, directly or through `via`.
    async fn hop(&self, host: &Host, hop: &Endpoint, via: Option<&SshHandle>) -> Result<SshHandle> {
        let label = format!("{}:{}", hop.hostname, hop.port);
        let rejection = Arc::new(std::sync::Mutex::new(None));
        let handler = ClientHandler {
            host: host.name.clone(),
            hostname: hop.hostname.clone(),
            port: hop.port,
            known: self.known.clone(),
            decider: self.decider.clone(),
            rejection: rejection.clone(),
        };
        let connected = match via {
            None => {
                let tcp = tokio::time::timeout(
                    self.config.connect_timeout,
                    TcpStream::connect((hop.hostname.as_str(), hop.port)),
                )
                .await
                .map_err(|_| Error::ConnectTimeout {
                    host: label.clone(),
                })?
                .map_err(|source| Error::Connect {
                    host: label.clone(),
                    source,
                })?;
                client::connect_stream(self.ssh.clone(), tcp, handler).await
            }
            Some(jump) => {
                let channel = jump
                    .channel_open_direct_tcpip(
                        hop.hostname.clone(),
                        u32::from(hop.port),
                        "127.0.0.1",
                        0,
                    )
                    .await
                    .map_err(|source| Error::Ssh {
                        host: label.clone(),
                        source,
                    })?;
                client::connect_stream(self.ssh.clone(), channel.into_stream(), handler).await
            }
        };
        let mut handle = connected.map_err(|source| {
            let reason = rejection.locked()
                .take();
            match reason {
                Some(reason) => Error::HostKeyRejected {
                    host: label.clone(),
                    reason,
                },
                None => Error::Ssh {
                    host: label.clone(),
                    source,
                },
            }
        })?;
        let socket = agent::socket_for(&hop.agent, self.config.default_agent.as_deref(), &label)?;
        agent::authenticate(&mut handle, &hop.user, &label, &socket).await?;
        Ok(handle)
    }
}
