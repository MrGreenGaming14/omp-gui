//! The [`PeerDirectory`]: saved peers (`peers.sqlite`) and what this node saw
//! of every node it met. A sighting is a hint; transports prove the key.

use crate::discovery::{NearbyPeer, NearbySource, merge_addresses};
use crate::transport::TransportKind;
use crate::wire::{self, Hello};
use crate::{NodeError, NodeId, Result};
use loa_util::LockExt;
use loa_util::time::now_ms;
use rusqlite::{Connection, OptionalExtension, ToSql, params};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Mutex;

loa_util::str_enum! {
    /// How a saved peer was found.
    pub DiscoverySource { Manual => "manual", Mdns => "mdns", Tailscale => "tailscale" }
}

/// A saved peer with what this node last observed about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerInfo {
    pub key: NodeId,
    pub name: Option<String>,
    pub addresses: Vec<SocketAddr>,
    pub source: DiscoverySource,
    pub online: bool,
    pub last_seen_ms: Option<i64>,
    pub app_version: Option<String>,
    /// Transport of the last connection, while online.
    pub transport: Option<TransportKind>,
    /// Whether it announced `omp/session/1`; `None` until seen.
    pub shared_protocol: Option<bool>,
    /// The protocol labels of its last hello.
    pub protocols: Vec<String>,
}

const COLUMNS: &str = "SELECT key, name, addresses, discovery_source, online, last_seen_ms, app_version FROM net_peers";

#[derive(Default)]
struct Seen {
    nearby: HashMap<NodeId, NearbyPeer>,
    transports: HashMap<NodeId, TransportKind>,
    protocols: HashMap<NodeId, Vec<String>>,
}

pub struct PeerDirectory {
    db: Mutex<Connection>,
    seen: Mutex<Seen>,
}

impl PeerDirectory {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS net_peers (key TEXT PRIMARY KEY NOT NULL, name TEXT, addresses TEXT NOT NULL DEFAULT '[]',
             discovery_source TEXT NOT NULL, online INTEGER NOT NULL DEFAULT 0, last_seen_ms INTEGER, app_version TEXT);",
        )?;
        Ok(Self { db: Mutex::new(db), seen: Mutex::default() })
    }

    /// Saves or refreshes a peer and drops it from the nearby list; a missing name or empty hints keep what was saved.
    pub fn save(&self, key: &NodeId, name: Option<&str>, addresses: &[SocketAddr], source: DiscoverySource) -> Result<()> {
        self.db.locked().execute(
            "INSERT INTO net_peers (key, name, addresses, discovery_source) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET name = COALESCE(excluded.name, net_peers.name),
               addresses = CASE WHEN excluded.addresses = '[]' THEN net_peers.addresses ELSE excluded.addresses END,
               discovery_source = excluded.discovery_source",
            params![key.to_string(), name, json(addresses), source.as_str()],
        )?;
        self.seen.locked().nearby.remove(key);
        Ok(())
    }

    /// Saves a nearby node under how it was found.
    pub fn save_nearby(&self, key: &NodeId) -> Result<()> {
        let nearby = self.seen.locked().nearby.get(key).cloned().ok_or_else(|| NodeError::UnknownPeer(key.to_string()))?;
        let source = match nearby.source {
            NearbySource::Mdns => DiscoverySource::Mdns,
            NearbySource::Tailscale => DiscoverySource::Tailscale,
            NearbySource::Incoming => DiscoverySource::Manual,
        };
        self.save(key, nearby.name.as_deref(), &nearby.addresses, source)?;
        if let Some(version) = &nearby.app_version {
            self.update(key, "app_version = ?2", &[version]);
        }
        Ok(())
    }

    fn lookup(&self, key: &NodeId) -> Option<PeerInfo> {
        self.db.locked().query_row(&format!("{COLUMNS} WHERE key = ?1"), [key.to_string()], row).optional().ok().flatten()
    }

    pub fn list(&self) -> Vec<PeerInfo> {
        let db = self.db.locked();
        let Ok(mut stmt) = db.prepare(&format!("{COLUMNS} ORDER BY key")) else { return Vec::new() };
        let peers: Vec<PeerInfo> = stmt.query_map([], row).map(|rows| rows.flatten().collect()).unwrap_or_default();
        let seen = self.seen.locked();
        peers.into_iter().map(|mut peer| {
            peer.transport = seen.transports.get(&peer.key).copied().filter(|_| peer.online);
            peer.protocols = seen.protocols.get(&peer.key).cloned().unwrap_or_default();
            peer.shared_protocol = seen.protocols.contains_key(&peer.key).then(|| peer.protocols.iter().any(|p| p.as_bytes() == wire::SESSION));
            peer
        }).collect()
    }

    pub fn keys(&self) -> Vec<NodeId> { self.list().into_iter().map(|p| p.key).collect() }

    pub fn nearby(&self) -> Vec<NearbyPeer> {
        let mut nearby: Vec<_> = self.seen.locked().nearby.values().cloned().collect();
        nearby.sort_by(|a, b| b.seen_ms.cmp(&a.seen_ms).then_with(|| a.key.cmp(&b.key)));
        nearby
    }

    /// Runs `UPDATE net_peers SET <set> WHERE key = ?1`; whether the peer exists.
    pub fn update(&self, key: &NodeId, set: &str, values: &[&dyn ToSql]) -> bool {
        let key = key.to_string();
        let params: Vec<&dyn ToSql> = std::iter::once(&key as &dyn ToSql).chain(values.iter().copied()).collect();
        self.db.locked().execute(&format!("UPDATE net_peers SET {set} WHERE key = ?1"), params.as_slice()).is_ok_and(|n| n != 0)
    }

    pub fn rename(&self, key: &NodeId, name: &str) -> bool { self.update(key, "name = ?2", &[&name]) }

    pub fn remove(&self, key: &NodeId) -> bool {
        self.seen.locked().transports.remove(key);
        self.db.locked().execute("DELETE FROM net_peers WHERE key = ?1", [key.to_string()]).is_ok_and(|n| n != 0)
    }

    /// A peer that stopped answering: offline, its last sighting kept.
    pub fn offline(&self, key: &NodeId) {
        self.update(key, "online = 0", &[]);
        self.seen.locked().transports.remove(key);
    }

    /// The user's name for a peer, its announced name, or the start of its key.
    pub fn name(&self, key: &NodeId) -> String {
        let saved = self.lookup(key).and_then(|p| p.name).filter(|n| !n.is_empty());
        saved.or_else(|| self.seen.locked().nearby.get(key).and_then(|n| n.name.clone())).unwrap_or_else(|| key.fmt_short().to_string())
    }

    /// Saved and nearby address hints of `key`.
    pub fn hints(&self, key: &NodeId) -> Vec<SocketAddr> {
        let saved = self.lookup(key).map(|p| p.addresses).unwrap_or_default();
        let nearby = self.seen.locked().nearby.get(key).map(|n| n.addresses.clone()).unwrap_or_default();
        merge_addresses(&nearby, saved, false)
    }

    /// A discovered node: a saved one gets its new addresses, any other is nearby.
    pub fn found(&self, peer: NearbyPeer) {
        match self.lookup(&peer.key) {
            Some(saved) => {
                let addresses = merge_addresses(&saved.addresses, peer.addresses, peer.source == NearbySource::Mdns);
                self.update(&peer.key, "addresses = ?2", &[&json(&addresses)]);
            }
            None => match self.seen.locked().nearby.entry(peer.key) {
                std::collections::hash_map::Entry::Occupied(mut known) => known.get_mut().merge(peer),
                std::collections::hash_map::Entry::Vacant(slot) => { slot.insert(peer); }
            },
        }
    }

    /// Records what a connection's hello showed about `peer`.
    pub fn observe(&self, peer: NodeId, hello: &Hello, kind: TransportKind) {
        let labels: Vec<String> = hello.protocols.iter().map(|p| format!("{}/{}", p.name, p.version)).collect();
        let known = self.update(&peer, "app_version = ?2, online = 1, last_seen_ms = ?3, name = CASE WHEN name IS NULL OR name = '' THEN NULLIF(?4, '') ELSE name END",
            &[&hello.app_version, &now_ms(), &hello.name]);
        {
            let mut seen = self.seen.locked();
            seen.transports.insert(peer, kind);
            seen.protocols.insert(peer, labels.clone());
        }
        if !known {
            let mut sighting = NearbyPeer::new(peer, NearbySource::Incoming, Some(hello.name.clone()).filter(|n| !n.is_empty()), Vec::new());
            sighting.app_version = Some(hello.app_version.clone());
            sighting.protocols = labels;
            self.found(sighting);
        }
    }
}

fn json(addresses: &[SocketAddr]) -> String { serde_json::to_string(addresses).unwrap_or_else(|_| "[]".into()) }

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PeerInfo> {
    let invalid = |i, e: String| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, NodeError::Protocol(e).into());
    let (key, addresses, source): (String, String, String) = (row.get(0)?, row.get(2)?, row.get(3)?);
    Ok(PeerInfo {
        key: key.parse::<NodeId>().map_err(|e| invalid(0, e.to_string()))?,
        name: row.get(1)?,
        addresses: serde_json::from_str(&addresses).map_err(|e| invalid(2, e.to_string()))?,
        source: source.parse().map_err(|e| invalid(3, e))?,
        online: row.get(4)?,
        last_seen_ms: row.get(5)?,
        app_version: row.get(6)?,
        transport: None,
        shared_protocol: None,
        protocols: Vec::new(),
    })
}
