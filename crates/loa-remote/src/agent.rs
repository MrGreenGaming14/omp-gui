//! Authentication through `ssh-agent` only (#96).
//!
//! Every identity the agent offers is tried, including FIDO2 `sk-ssh-ed25519`
//! and `sk-ecdsa-sha2-nistp256` keys and OpenSSH certificates: the agent (and
//! the hardware token behind it) produces the signature, so the private key
//! never enters this process. There is no other authentication path in the
//! crate; `clippy.toml` bans the private key loaders.

use std::path::{Path, PathBuf};

use russh::client::{self, AuthResult};
use russh::keys::agent::AgentIdentity;
use russh::keys::agent::client::AgentClient;

use crate::error::{Error, Result};
use crate::ssh_config::AgentSocket;

/// Picks the agent socket for a hop: `IdentityAgent` when set, else the
/// app's default socket.
pub fn socket_for(
    agent: &AgentSocket,
    default: Option<&Path>,
    host: &str,
) -> Result<PathBuf> {
    match agent {
        AgentSocket::Path(p) => Ok(p.clone()),
        AgentSocket::Default => default
            .map(Path::to_path_buf)
            .ok_or_else(|| Error::NoAgent {
                host: host.to_string(),
            }),
        AgentSocket::Disabled => Err(Error::NoAgent {
            host: host.to_string(),
        }),
    }
}

/// Authenticates `handle` as `user` with the identities held by the agent
/// listening on `socket`.
pub async fn authenticate<H: client::Handler>(
    handle: &mut client::Handle<H>,
    user: &str,
    host: &str,
    socket: &Path,
) -> Result<()> {
    let agent_err = |message: String| Error::Agent {
        path: socket.to_path_buf(),
        message,
    };
    #[cfg(unix)]
    let connected = AgentClient::connect_uds(socket).await;
    #[cfg(windows)]
    let connected = AgentClient::connect_named_pipe(socket).await;
    let mut agent = connected.map_err(|e| agent_err(e.to_string()))?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|e| agent_err(e.to_string()))?;
    let mut tried = 0;
    for identity in identities {
        tried += 1;
        let outcome = match identity {
            AgentIdentity::PublicKey { key, .. } => {
                let hash = if key.algorithm().is_rsa() {
                    handle
                        .best_supported_rsa_hash()
                        .await
                        .map_err(|source| Error::Ssh {
                            host: host.to_string(),
                            source,
                        })?
                        .flatten()
                } else {
                    None
                };
                handle
                    .authenticate_publickey_with(user, key, hash, &mut agent)
                    .await
            }
            AgentIdentity::Certificate { certificate, .. } => {
                handle
                    .authenticate_certificate_with(user, certificate, None, &mut agent)
                    .await
            }
        };
        match outcome {
            Ok(AuthResult::Success) => return Ok(()),
            Ok(AuthResult::Failure { .. }) => {}
            // The agent refused to sign (a FIDO2 touch not given, a key
            // needing confirmation that was declined): try the next one.
            Err(e) => tracing::debug!(host, user, error = %e, "agent could not sign"),
        }
    }
    Err(Error::AuthFailed {
        user: user.to_string(),
        host: host.to_string(),
        tried,
    })
}
