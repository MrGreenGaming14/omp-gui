//! App hosted `remote` MCP server: `remote_bash` and `remote_file` over SSH.
//!
//! Hosts come from `~/.ssh/config` ([`ssh_config`]) or are added by hand
//! ([`hosts`]). Connections go through `russh` ([`connection`]), authenticate
//! only through `ssh-agent` ([`agent`]), and check host keys against
//! `known_hosts` ([`known_hosts`]) with a decision port for new or changed
//! keys ([`ports`]). Per host limits ([`limits`]) are enforced before anything
//! is sent, and every action lands in the audit log ([`audit`]).
//! [`RemoteService`] ties these together and [`mcp::RemoteMcp`] serves it.
//!
//! Private keys: this crate never opens a private key file. The only
//! authentication path is [`agent::authenticate`], which signs through an
//! `ssh-agent` socket. `clippy.toml` in this crate disallows every russh and
//! ssh-key function that loads or decodes private key material.

pub mod agent;
pub mod audit;
pub mod connection;
mod error;
mod exec;
pub mod hosts;
pub mod known_hosts;
pub mod limits;
pub mod mcp;
pub mod ports;
mod service;
mod sftp;
pub mod ssh_config;

pub use error::{Error, Result};
pub use exec::{ExecEnd, ExecOutput};
pub use service::{
    BashRequest, DirEntry, EntryKind, FileOp, FileOutput, FileRequest, RemoteService,
    RemoteServiceConfig,
};
pub use sftp::WriteMode;
