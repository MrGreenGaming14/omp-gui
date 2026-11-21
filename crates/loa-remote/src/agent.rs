//! Authentication through `ssh-agent` only: every identity the agent offers is tried (FIDO2 and
//! certificates included) and the agent signs, so no private key enters this process.

use std::path::{Path, PathBuf};

use russh::client::{self, AuthResult};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;

use crate::error::{Error, Result};
use crate::hosts::AgentSocket;

/// The agent socket for a hop: `IdentityAgent` when set, else the app's default.
pub fn socket_for(agent: &AgentSocket, default: Option<&Path>, host: &str) -> Result<PathBuf> {
    let no_agent = || Error::NoAgent { host: host.to_owned() };
    match agent {
        AgentSocket::Path(p) => Ok(p.clone()),
        AgentSocket::Default => default.map(Path::to_path_buf).ok_or_else(no_agent),
        AgentSocket::Disabled => Err(no_agent()),
    }
}

/// Authenticates `handle` as `user` with the identities of the agent on `socket`.
pub async fn authenticate<H: client::Handler>(handle: &mut client::Handle<H>, user: &str, host: &str, socket: &Path) -> Result<()> {
    let agent_err = |e: russh::keys::Error| Error::Agent { path: socket.to_path_buf(), message: e.to_string() };
    #[cfg(unix)]
    let connected = AgentClient::connect_uds(socket).await;
    #[cfg(windows)]
    let connected = AgentClient::connect_named_pipe(socket).await;
    let mut agent = connected.map_err(agent_err)?;
    let identities = agent.request_identities().await.map_err(agent_err)?;
    let tried = identities.len();
    for identity in identities {
        let outcome = match identity {
            AgentIdentity::PublicKey { key, .. } => {
                let hash = match key.algorithm().is_rsa() {
                    true => handle.best_supported_rsa_hash().await.map_err(|source| Error::Ssh { host: host.to_owned(), source })?.flatten(),
                    false => None,
                };
                handle.authenticate_publickey_with(user, key, hash, &mut agent).await
            }
            AgentIdentity::Certificate { certificate, .. } => handle.authenticate_certificate_with(user, certificate, None, &mut agent).await,
        };
        match outcome {
            Ok(AuthResult::Success) => return Ok(()),
            Ok(AuthResult::Failure { .. }) => {}
            // The agent refused to sign (a FIDO2 touch not given): try the next identity.
            Err(e) => tracing::debug!(host, user, error = %e, "agent could not sign"),
        }
    }
    Err(Error::AuthFailed { user: user.to_owned(), host: host.to_owned(), tried })
}
