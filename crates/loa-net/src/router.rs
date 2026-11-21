//! Dispatches accepted connections by negotiated ALPN to protocol handlers,
//! after asking the authorization port.

use crate::NodeId;
use crate::alpn::{self, Alpn};
use crate::conn::Connection;
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::Arc;

/// QUIC/TCP application close code sent when the authorization port refuses.
pub const CLOSE_DENIED: u32 = 0x403;
/// Close code sent when no handler is registered for the negotiated ALPN.
pub const CLOSE_NO_HANDLER: u32 = 0x404;

/// Outcome of an authorization check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny(String),
}

/// Port: decides whether a peer may open a protocol, consulted for every
/// incoming connection before a handler sees it. `loa-app` backs it with the
/// grant check of `loa-trust`.
#[async_trait]
pub trait Authorizer: Send + Sync {
    async fn authorize(&self, peer: &NodeId, alpn: &Alpn) -> Decision;
}

/// Allows every peer. For tests and for nodes that check grants inside handlers.
pub struct AllowAll;

#[async_trait]
impl Authorizer for AllowAll {
    async fn authorize(&self, _peer: &NodeId, _alpn: &Alpn) -> Decision {
        Decision::Allow
    }
}

/// Serves one protocol version on accepted connections.
#[async_trait]
pub trait ProtocolHandler: Send + Sync {
    /// Runs for the life of the connection, on its own task.
    async fn handle(&self, conn: Connection);
}

/// Handlers keyed by protocol name and version, plus the authorization port.
#[derive(Clone)]
pub struct Router {
    handlers: BTreeMap<Alpn, Arc<dyn ProtocolHandler>>,
    authorizer: Arc<dyn Authorizer>,
}

impl Router {
    pub fn new(authorizer: Arc<dyn Authorizer>) -> Self {
        Self { handlers: BTreeMap::new(), authorizer }
    }

    /// Registers `handler` for `protocol` at `version`, replacing any earlier one.
    pub fn handler(mut self, protocol: &str, version: u32, handler: Arc<dyn ProtocolHandler>) -> Self {
        self.handlers.insert(Alpn::new(protocol, version), handler);
        self
    }

    /// Every registered label in negotiation order (highest version first per protocol).
    pub fn alpns(&self) -> Vec<Vec<u8>> {
        alpn::server_order(self.handlers.keys().cloned())
    }

    /// Versions registered for `protocol`.
    pub fn versions(&self, protocol: &str) -> Vec<u32> {
        self.handlers.keys().filter(|a| a.protocol == protocol).map(|a| a.version).collect()
    }

    /// Every registered protocol with its versions.
    pub fn protocols(&self) -> BTreeMap<String, Vec<u32>> {
        let mut out: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for a in self.handlers.keys() {
            out.entry(a.protocol.clone()).or_default().push(a.version);
        }
        out
    }

    /// Checks `conn` with the authorization port and runs its handler.
    /// Refused connections are closed with [`CLOSE_DENIED`].
    pub async fn dispatch(&self, conn: Connection) {
        let peer = conn.remote();
        let alpn = conn.alpn().clone();
        if let Decision::Deny(reason) = self.authorizer.authorize(&peer, &alpn).await {
            tracing::debug!(%peer, %alpn, %reason, "connection refused");
            conn.close(CLOSE_DENIED, reason.as_bytes());
            return;
        }
        match self.handlers.get(&alpn) {
            Some(handler) => handler.handle(conn).await,
            None => conn.close(CLOSE_NO_HANDLER, b"no handler"),
        }
    }
}
