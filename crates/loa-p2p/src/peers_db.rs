//! The saved peers (`peers.sqlite`).

use crate::{NodeError, NodeId, Result};
use rusqlite::{Connection, OptionalExtension, ToSql, params};
use std::net::SocketAddr;
use std::path::Path;

loa_util::str_enum! {
    /// How a saved peer was found.
    pub DiscoverySource { Manual => "manual", Mdns => "mdns", Tailscale => "tailscale" }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub key: NodeId,
    pub name: Option<String>,
    pub addresses: Vec<SocketAddr>,
    pub source: DiscoverySource,
    pub online: bool,
    pub last_seen_ms: Option<i64>,
    pub app_version: Option<String>,
}

pub struct PeerStore(Connection);

const COLUMNS: &str = "SELECT key, name, addresses, discovery_source, online, last_seen_ms, app_version FROM net_peers";

impl PeerStore {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS net_peers (key TEXT PRIMARY KEY NOT NULL, name TEXT, addresses TEXT NOT NULL DEFAULT '[]',
             discovery_source TEXT NOT NULL, online INTEGER NOT NULL DEFAULT 0, last_seen_ms INTEGER, app_version TEXT);",
        )?;
        Ok(Self(db))
    }

    /// Saves or refreshes a peer; a missing name or empty hints keep what was saved.
    pub fn add(&self, key: &NodeId, name: Option<&str>, addresses: &[SocketAddr], source: DiscoverySource) -> Result<()> {
        self.0.execute(
            "INSERT INTO net_peers (key, name, addresses, discovery_source) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET name = COALESCE(excluded.name, net_peers.name),
               addresses = CASE WHEN excluded.addresses = '[]' THEN net_peers.addresses ELSE excluded.addresses END,
               discovery_source = excluded.discovery_source",
            params![key.to_string(), name, json(addresses), source.as_str()],
        )?;
        Ok(())
    }

    pub fn lookup(&self, key: &NodeId) -> Option<Peer> {
        self.0.query_row(&format!("{COLUMNS} WHERE key = ?1"), [key.to_string()], row).optional().ok().flatten()
    }

    pub fn list(&self) -> Vec<Peer> {
        let Ok(mut stmt) = self.0.prepare(&format!("{COLUMNS} ORDER BY key")) else { return Vec::new() };
        stmt.query_map([], row).map(|rows| rows.flatten().collect()).unwrap_or_default()
    }

    /// Runs `UPDATE net_peers SET <set> WHERE key = ?1`; whether the peer exists.
    pub fn update(&self, key: &NodeId, set: &str, values: &[&dyn ToSql]) -> bool {
        let key = key.to_string();
        let params: Vec<&dyn ToSql> = std::iter::once(&key as &dyn ToSql).chain(values.iter().copied()).collect();
        self.0.execute(&format!("UPDATE net_peers SET {set} WHERE key = ?1"), params.as_slice()).is_ok_and(|n| n != 0)
    }

    /// Online observations refresh `last_seen_ms`; going offline keeps it.
    pub fn presence(&self, key: &NodeId, online: bool) -> bool {
        self.update(key, "online = ?2, last_seen_ms = CASE WHEN ?2 THEN ?3 ELSE last_seen_ms END", &[&online, &loa_util::time::now_ms()])
    }

    pub fn set_addresses(&self, key: &NodeId, addresses: &[SocketAddr]) -> bool { self.update(key, "addresses = ?2", &[&json(addresses)]) }

    pub fn remove(&self, key: &NodeId) -> Result<bool> { Ok(self.0.execute("DELETE FROM net_peers WHERE key = ?1", [key.to_string()])? != 0) }
}

fn json(addresses: &[SocketAddr]) -> String { serde_json::to_string(addresses).unwrap_or_else(|_| "[]".into()) }

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Peer> {
    let invalid = |i, e: String| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, NodeError::Protocol(e).into());
    let key: String = row.get(0)?;
    let addresses: String = row.get(2)?;
    let source: String = row.get(3)?;
    Ok(Peer {
        key: key.parse::<NodeId>().map_err(|e| invalid(0, e.to_string()))?,
        name: row.get(1)?,
        addresses: serde_json::from_str(&addresses).map_err(|e| invalid(2, e.to_string()))?,
        source: source.parse().map_err(|e| invalid(3, e))?,
        online: row.get(4)?,
        last_seen_ms: row.get(5)?,
        app_version: row.get(6)?,
    })
}
