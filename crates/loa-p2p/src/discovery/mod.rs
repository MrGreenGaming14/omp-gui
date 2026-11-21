//! Finding nodes: the [`Discoverer`] port with mDNS and tailnet adapters. A
//! discovery is a hint; the transport proves the key on every connection.

mod mdns;
mod tailnet;

pub use mdns::{Announce, Mdns};
pub use tailnet::{NodeProbe, Tailnet};

use crate::NodeId;
use async_trait::async_trait;
use std::net::SocketAddr;

/// Whether an optional discovery source works here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Turned off, or its tool isn't installed.
    Off(String),
    Active(String),
    Failed(String),
}

/// How an unsaved node became known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NearbySource {
    Mdns,
    Tailscale,
    /// It connected to this node.
    Incoming,
}

/// A node seen but not saved as a peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NearbyPeer {
    pub key: NodeId,
    pub name: Option<String>,
    pub addresses: Vec<SocketAddr>,
    pub source: NearbySource,
    pub seen_ms: i64,
    pub app_version: Option<String>,
    /// The protocol labels it announced, when it connected or answered a probe.
    pub protocols: Vec<String>,
}

impl NearbyPeer {
    pub fn new(key: NodeId, source: NearbySource, name: Option<String>, addresses: Vec<SocketAddr>) -> Self {
        Self { key, name, addresses, source, seen_ms: loa_util::time::now_ms(), app_version: None, protocols: Vec::new() }
    }

    /// Folds a later sighting in: mDNS replaces addresses, the others add to them.
    pub fn merge(&mut self, seen: NearbyPeer) {
        if seen.source != NearbySource::Incoming {
            self.source = seen.source;
        }
        self.seen_ms = seen.seen_ms;
        self.name = seen.name.or(self.name.take());
        self.app_version = seen.app_version.or(self.app_version.take());
        if !seen.protocols.is_empty() {
            self.protocols = seen.protocols;
        }
        self.addresses = merge_addresses(&self.addresses, seen.addresses, seen.source == NearbySource::Mdns);
    }
}

/// `found` first, then the `old` ones it lacks unless `replace`; `old` when `found` is empty.
pub(crate) fn merge_addresses(old: &[SocketAddr], mut found: Vec<SocketAddr>, replace: bool) -> Vec<SocketAddr> {
    if found.is_empty() {
        return old.to_vec();
    }
    if !replace {
        found.extend(old.iter().filter(|a| !found.contains(a)).copied().collect::<Vec<_>>());
    }
    found
}

/// Port: a source of nearby nodes.
#[async_trait]
pub trait Discoverer: Send {
    /// The next node found; `None` once the source is done.
    async fn next(&mut self) -> Option<NearbyPeer>;
    fn availability(&self) -> Availability;
}
