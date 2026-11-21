//! App hosted `remote` tools: `remote_bash` and `remote_file` over SSH.
//!
//! Hosts come from the app config resolved through an [`resolve::SshResolver`] (`ssh -G`).
//! Connections go through `russh`, authenticate only through `ssh-agent` ([`agent`]) and check
//! host keys against `known_hosts` with a decision port ([`ports`]). Per host limits run before
//! anything is sent and every action lands in an [`audit::AuditSink`]. [`RemoteService`] ties
//! these together; [`tools`] exposes it as a `ToolSet`, served over loopback HTTP by [`http`].
//!
//! This crate never opens a private key file: `clippy.toml` bans every russh and ssh-key
//! function that loads or decodes private key material.

pub mod agent;
pub mod audit;
pub mod config;
pub mod connection;
mod error;
mod exec;
pub mod hosts;
pub mod http;
pub mod known_hosts;
pub mod limits;
pub mod ports;
pub mod pty;
pub mod resolve;
mod service;
mod sftp;
pub mod tools;

pub use error::{Error, Result};
pub use exec::{ExecEnd, ExecOutput};
pub use service::{BashRequest, DirEntry, EntryKind, FileOp, FileOutput, FileRequest, RemoteService, RemoteServiceConfig, TERMINAL_SESSION, apply_edit};
pub use sftp::WriteMode;
