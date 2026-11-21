//! Authenticated peer connections, discovery and control traffic.

/// The public ed25519 node key used as the network address.
pub type NodeId = iroh::PublicKey;

pub mod alpn;
pub mod conn;
pub mod control;
pub mod discovery;
pub mod endpoint;
pub mod error;
pub mod peers;
pub mod network;
pub mod router;
pub mod tailscale;
pub mod tcp;

pub use error::{NetError, Result};
/// Default port of every node, UDP and TCP; defined once in loa-config.
pub use loa_config::DEFAULT_PORT;
