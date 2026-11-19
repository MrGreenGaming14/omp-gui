//! Persistent peer directory. A discovery hint is not proof of identity: the
//! transport authenticates the key again when a connection is established.

use crate::error::{NetError, Result};
use crate::NodeId;
use rusqlite::{params, Connection, OptionalExtension};
use std::net::SocketAddr;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoverySource {
    Manual,
    Mdns,
    Tailscale,
}

impl DiscoverySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Mdns => "mdns",
            Self::Tailscale => "tailscale",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "manual" => Ok(Self::Manual),
            "mdns" => Ok(Self::Mdns),
            "tailscale" => Ok(Self::Tailscale),
            _ => Err(NetError::Protocol(format!("unknown discovery source: {value}"))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub key: NodeId,
    pub name: Option<String>,
    pub addresses: Vec<SocketAddr>,
    pub discovery_source: DiscoverySource,
    pub online: bool,
    /// Unix epoch milliseconds, supplied by the caller when presence is observed.
    pub last_seen_ms: Option<i64>,
    pub app_version: Option<String>,
}

pub struct PeerStore {
    db: Connection,
}

impl PeerStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Connection::open(path)?;
        Self::from_connection(db)
    }

    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(db: Connection) -> Result<Self> {
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS net_peers (
                key TEXT PRIMARY KEY NOT NULL,
                name TEXT,
                addresses TEXT NOT NULL DEFAULT '[]',
                discovery_source TEXT NOT NULL,
                online INTEGER NOT NULL DEFAULT 0,
                last_seen_ms INTEGER,
                app_version TEXT
            );",
        )?;
        Ok(Self { db })
    }

    /// Adds a key only after parsing it as an iroh public key. Empty address
    /// hints and missing names leave existing hints intact on rediscovery.
    pub fn add_peer(
        &self,
        key: &str,
        name: Option<&str>,
        addresses: &[SocketAddr],
        source: DiscoverySource,
    ) -> Result<Peer> {
        let key = parse_key(key)?;
        let addresses = serde_json::to_string(addresses)
            .map_err(|e| NetError::Protocol(format!("encoding peer addresses: {e}")))?;
        self.db.execute(
            "INSERT INTO net_peers (key, name, addresses, discovery_source)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET
               name = COALESCE(excluded.name, net_peers.name),
               addresses = CASE WHEN excluded.addresses = '[]' THEN net_peers.addresses ELSE excluded.addresses END,
               discovery_source = excluded.discovery_source",
            params![key.to_string(), name, addresses, source.as_str()],
        )?;
        self.lookup(&key)?.ok_or_else(|| NetError::Protocol("newly added peer missing".into()))
    }

    pub fn lookup(&self, key: &NodeId) -> Result<Option<Peer>> {
        let mut stmt = self.db.prepare(
            "SELECT key, name, addresses, discovery_source, online, last_seen_ms, app_version
             FROM net_peers WHERE key = ?1",
        )?;
        let row = stmt.query_row([key.to_string()], read_row).optional()?;
        row.map(decode_row).transpose()
    }

    pub fn list(&self) -> Result<Vec<Peer>> {
        let mut stmt = self.db.prepare(
            "SELECT key, name, addresses, discovery_source, online, last_seen_ms, app_version
             FROM net_peers ORDER BY key",
        )?;
        let rows = stmt.query_map([], read_row)?;
        rows.map(|row| decode_row(row?)).collect()
    }

    /// Online observations refresh last_seen; going offline keeps the last
    /// time the peer was actually observed online.
    pub fn update_presence(&self, key: &NodeId, online: bool, observed_ms: i64) -> Result<bool> {
        Ok(self.db.execute(
            "UPDATE net_peers SET online = ?2,
                last_seen_ms = CASE WHEN ?2 THEN ?3 ELSE last_seen_ms END
             WHERE key = ?1",
            params![key.to_string(), online, observed_ms],
        )? != 0)
    }

    pub fn update_app_version(&self, key: &NodeId, version: &str) -> Result<bool> {
        Ok(self.db.execute(
            "UPDATE net_peers SET app_version = ?2 WHERE key = ?1",
            params![key.to_string(), version],
        )? != 0)
    }

    /// Sets the user's name for a peer. Returns whether the peer exists.
    pub fn rename(&self, key: &NodeId, name: &str) -> Result<bool> {
        Ok(self.db.execute("UPDATE net_peers SET name = ?2 WHERE key = ?1", params![key.to_string(), name])? != 0)
    }

    /// Names a peer after its own hello only while the user hasn't named it.
    pub fn name_if_unset(&self, key: &NodeId, name: &str) -> Result<bool> {
        Ok(self.db.execute(
            "UPDATE net_peers SET name = ?2 WHERE key = ?1 AND (name IS NULL OR name = '')",
            params![key.to_string(), name],
        )? != 0)
    }

    /// Forgets a peer. Returns whether it existed.
    pub fn remove(&self, key: &NodeId) -> Result<bool> {
        Ok(self.db.execute("DELETE FROM net_peers WHERE key = ?1", params![key.to_string()])? != 0)
    }

    /// Replaces a known peer's address hints, keeping how it was found.
    pub fn set_addresses(&self, key: &NodeId, addresses: &[SocketAddr]) -> Result<bool> {
        let addresses = serde_json::to_string(addresses)
            .map_err(|e| NetError::Protocol(format!("encoding peer addresses: {e}")))?;
        Ok(self.db.execute("UPDATE net_peers SET addresses = ?2 WHERE key = ?1", params![key.to_string(), addresses])? != 0)
    }
}

type PeerRow = (String, Option<String>, String, String, bool, Option<i64>, Option<String>);

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PeerRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
}

fn decode_row((key, name, addresses, source, online, last_seen_ms, app_version): PeerRow) -> Result<Peer> {
    Ok(Peer {
        key: parse_key(&key)?,
        name,
        addresses: serde_json::from_str(&addresses)
            .map_err(|e| NetError::Protocol(format!("invalid peer addresses: {e}")))?,
        discovery_source: DiscoverySource::parse(&source)?,
        online,
        last_seen_ms,
        app_version,
    })
}

fn parse_key(key: &str) -> Result<NodeId> {
    key.parse().map_err(|e| NetError::InvalidKey(format!("{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> NodeId {
        iroh::SecretKey::from_bytes(&[7; 32]).public()
    }

    #[test]
    fn rename_hello_name_and_remove() {
        let store = PeerStore::in_memory().unwrap();
        let id = key();
        store.add_peer(&id.to_string(), None, &[], DiscoverySource::Manual).unwrap();
        assert!(store.name_if_unset(&id, "laptop").unwrap());
        assert!(store.rename(&id, "Alice's laptop").unwrap());
        assert!(!store.name_if_unset(&id, "laptop").unwrap());
        assert_eq!(store.lookup(&id).unwrap().unwrap().name.as_deref(), Some("Alice's laptop"));
        assert!(store.remove(&id).unwrap());
        assert!(!store.remove(&id).unwrap());
        assert!(store.list().unwrap().is_empty());
    }

    #[test]
    fn validates_keys_and_retains_observed_metadata_on_rediscovery() {
        let store = PeerStore::in_memory().unwrap();
        assert!(matches!(store.add_peer("not-a-key", None, &[], DiscoverySource::Manual), Err(NetError::InvalidKey(_))));
        assert!(store.list().unwrap().is_empty());
        let id = key();
        let address = "127.0.0.1:31415".parse().unwrap();
        store.add_peer(&id.to_string(), Some("Alice"), &[address], DiscoverySource::Mdns).unwrap();
        assert!(store.update_presence(&id, true, 123).unwrap());
        assert!(store.update_app_version(&id, "1.2.3").unwrap());
        store.add_peer(&id.to_string(), None, &[], DiscoverySource::Tailscale).unwrap();
        let peer = store.lookup(&id).unwrap().unwrap();
        assert_eq!(peer.name.as_deref(), Some("Alice"));
        assert_eq!(peer.addresses, vec![address]);
        assert_eq!(peer.discovery_source, DiscoverySource::Tailscale);
        assert_eq!(peer.app_version.as_deref(), Some("1.2.3"));
        store.update_presence(&id, false, 456).unwrap();
        assert_eq!(store.lookup(&id).unwrap().unwrap().last_seen_ms, Some(123));
        assert!(!store.update_presence(&iroh::SecretKey::from_bytes(&[8; 32]).public(), true, 456).unwrap());
    }

    #[test]
    fn persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("peers.sqlite");
        let id = key();
        {
            let store = PeerStore::open(&file).unwrap();
            store.add_peer(&id.to_string(), None, &[], DiscoverySource::Manual).unwrap();
            store.update_app_version(&id, "2.0").unwrap();
            store.update_presence(&id, true, 42).unwrap();
        }
        let store = PeerStore::open(&file).unwrap();
        let peers = store.list().unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].key, id);
        assert_eq!(peers[0].last_seen_ms, Some(42));
        assert_eq!(peers[0].app_version.as_deref(), Some("2.0"));
    }
}
