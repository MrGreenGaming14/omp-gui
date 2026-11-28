//! Grant definitions in SQLite: grants issued, grants received, revocations.
//! Tokens live minutes; the issuer keeps each definition and mints a fresh
//! token whenever the holder asks while it is active. Revoking stops renewal.

use crate::key::{NodeIdentity, PublicKey};
use crate::scope::Scope;
use crate::token::{Chain, GrantId, Revocations, attenuate, mint};
use crate::{Result, TrustError};
use loa_util::LockExt;
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS grants_issued (id TEXT PRIMARY KEY, holder TEXT NOT NULL, spec TEXT NOT NULL, parent TEXT,
    created_ms INTEGER NOT NULL, revoked_ms INTEGER);
CREATE TABLE IF NOT EXISTS grants_received (id TEXT PRIMARY KEY, issuer TEXT NOT NULL, root TEXT NOT NULL, token BLOB NOT NULL,
    expires_ms INTEGER NOT NULL, reshare INTEGER NOT NULL, refused_ms INTEGER);
CREATE TABLE IF NOT EXISTS grants_revocations (id TEXT PRIMARY KEY, issuer TEXT NOT NULL, revoked_ms INTEGER NOT NULL);
";
const ISSUED: &str = "SELECT id, holder, spec, parent, created_ms, revoked_ms FROM grants_issued";
const RECEIVED: &str = "SELECT id, issuer, root, token, expires_ms, reshare, refused_ms FROM grants_received";

/// What a new grant gives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantSpec {
    pub scopes: Vec<Scope>,
    /// Lifetime of each token; renewal hands out a new one.
    pub ttl_ms: i64,
    /// Whether the holder may re-share a narrower grant.
    pub reshare: bool,
    /// When the grant ends: no token outlives it. `None` renews until revoked.
    #[serde(default)]
    pub until_ms: Option<i64>,
}

impl GrantSpec {
    fn token_expiry(&self, id: &GrantId, now_ms: i64) -> Result<i64> {
        let expires_ms = now_ms.saturating_add(self.ttl_ms);
        match self.until_ms {
            Some(until) if until <= now_ms => Err(TrustError::Expired(id.clone())),
            until => Ok(until.map_or(expires_ms, |until| expires_ms.min(until))),
        }
    }
}

/// How a holder narrows a grant it re-shares: rights are the intersection of
/// the parent's and these, so a re-share never widens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Narrowing {
    /// Restrict to these scopes; `None` keeps the parent's.
    pub scopes: Option<Vec<Scope>>,
    /// Lifetime of each token, capped by the parent token's expiry.
    pub ttl_ms: i64,
    pub reshare: bool,
}

/// The stored definition of an issued grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Definition {
    Root(GrantSpec),
    Reshare(Narrowing),
}

/// A token minted for a holder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedGrant {
    pub id: GrantId,
    pub holder: PublicKey,
    pub token: Vec<u8>,
    pub expires_ms: i64,
}

/// A grant this node holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedGrant {
    pub id: GrantId,
    /// Who renews it: the root, or the peer that re-shared it.
    pub issuer: PublicKey,
    /// The node whose resources it covers and that verifies it.
    pub root: PublicKey,
    pub token: Vec<u8>,
    pub expires_ms: i64,
    pub reshare: bool,
    /// When the issuer refused to renew it.
    pub refused_ms: Option<i64>,
}

impl ReceivedGrant {
    /// The rights its root signed into it; see [`crate::token_scopes`].
    pub fn scopes(&self) -> Result<Vec<Scope>> { crate::token::token_scopes(&self.token, &self.root) }
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        let (id, issuer, root, token, expires_ms, reshare, refused_ms) = (r.get(0)?, key(r, 1)?, key(r, 2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?);
        Ok(Self { id: GrantId(id), issuer, root, token, expires_ms, reshare, refused_ms })
    }
}

/// A grant this node issued, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedRecord {
    pub id: GrantId,
    pub holder: PublicKey,
    /// The rights given; `None` for a re-share that keeps its parent's.
    pub scopes: Option<Vec<Scope>>,
    pub ttl_ms: i64,
    pub reshare: bool,
    /// When a root grant ends; `None` until revoked (and for re-shares).
    pub until_ms: Option<i64>,
    /// The received grant a re-share was cut from.
    pub parent: Option<GrantId>,
    pub created_ms: i64,
    pub revoked_ms: Option<i64>,
}

/// One `grants_issued` row: (id, holder, definition, parent, created, revoked).
type IssuedRow = (GrantId, PublicKey, Definition, Option<GrantId>, i64, Option<i64>);

fn issued_row(r: &Row<'_>) -> rusqlite::Result<IssuedRow> {
    let definition = serde_json::from_str(&r.get::<_, String>(2)?).map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, Type::Text, e.into()))?;
    Ok((GrantId(r.get(0)?), key(r, 1)?, definition, r.get::<_, Option<String>>(3)?.map(GrantId), r.get(4)?, r.get(5)?))
}

fn key(r: &Row<'_>, i: usize) -> rusqlite::Result<PublicKey> {
    PublicKey::from_hex(&r.get::<_, String>(i)?).map_err(|e| rusqlite::Error::FromSqlConversionFailure(i, Type::Text, e.into()))
}

/// One node's grant tables.
pub struct GrantStore {
    db: Mutex<Connection>,
}

impl std::fmt::Debug for GrantStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("GrantStore").finish_non_exhaustive() }
}

impl GrantStore {
    pub fn open(path: &Path) -> Result<Self> { Self::with_connection(Connection::open(path)?) }
    /// Uses an open connection, creating the tables if needed.
    pub fn with_connection(db: Connection) -> Result<Self> {
        db.execute_batch(SCHEMA)?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn insert_issued(&self, issued: &IssuedGrant, definition: &Definition, parent: Option<&GrantId>, now_ms: i64) -> Result<IssuedGrant> {
        self.db.locked().execute(
            "INSERT INTO grants_issued (id, holder, spec, parent, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![issued.id.0, issued.holder.to_string(), serde_json::to_string(definition)?, parent.map(|p| &p.0), now_ms],
        )?;
        Ok(issued.clone())
    }

    /// The holder, definition and parent of an active grant this node issued.
    fn active(&self, id: &GrantId) -> Result<(PublicKey, Definition, Option<GrantId>)> {
        let row = self.db.locked().query_row(&format!("{ISSUED} WHERE id = ?1"), params![id.0], issued_row).optional()?;
        match row.ok_or_else(|| TrustError::UnknownGrant(id.clone()))? {
            (.., Some(_)) => Err(TrustError::Revoked(id.clone())),
            (_, holder, definition, parent, ..) => Ok((holder, definition, parent)),
        }
    }

    fn mint_root(issuer: &NodeIdentity, id: &GrantId, holder: PublicKey, spec: &GrantSpec, now_ms: i64) -> Result<IssuedGrant> {
        spec.scopes.iter().try_for_each(Scope::validate)?;
        let expires_ms = spec.token_expiry(id, now_ms)?;
        let token = mint(issuer, id, &holder, &spec.scopes, expires_ms, spec.reshare)?;
        Ok(IssuedGrant { id: id.clone(), holder, token, expires_ms })
    }

    /// Creates a grant for `holder` and mints its first token.
    pub fn issue(&self, issuer: &NodeIdentity, holder: PublicKey, spec: GrantSpec, now_ms: i64) -> Result<IssuedGrant> {
        if spec.scopes.is_empty() {
            return Err(TrustError::NoScopes);
        }
        check_ttl(spec.ttl_ms)?;
        let issued = Self::mint_root(issuer, &GrantId::random(), holder, &spec, now_ms)?;
        self.insert_issued(&issued, &Definition::Root(spec), None, now_ms)
    }

    /// Mints a fresh token for an active grant this node issued, for its holder only.
    pub fn renew(&self, issuer: &NodeIdentity, id: &GrantId, requester: &PublicKey, now_ms: i64) -> Result<IssuedGrant> {
        match self.active(id)? {
            (holder, ..) if holder != *requester => Err(TrustError::NotHolder(id.clone())),
            (holder, Definition::Root(spec), None) => Self::mint_root(issuer, id, holder, &spec, now_ms),
            (holder, Definition::Reshare(narrowing), Some(parent)) => self.attenuate_from(issuer, &parent, id.clone(), holder, &narrowing, now_ms),
            _ => Err(TrustError::Malformed(format!("grant {id} has a parent only when it is a re-share"))),
        }
    }

    /// Moves the end of a root grant to `until_ms` (`None`: until revoked) and
    /// mints a token at once, so the holder sees the change before its next renewal.
    pub fn extend(&self, issuer: &NodeIdentity, id: &GrantId, until_ms: Option<i64>, now_ms: i64) -> Result<IssuedGrant> {
        let (holder, Definition::Root(mut spec), None) = self.active(id)? else {
            return Err(TrustError::Malformed(format!("only root grants can be extended, {id} is a re-share")));
        };
        spec.until_ms = until_ms;
        let issued = Self::mint_root(issuer, id, holder, &spec, now_ms)?;
        self.db.locked().execute("UPDATE grants_issued SET spec = ?2 WHERE id = ?1", params![id.0, serde_json::to_string(&Definition::Root(spec))?])?;
        Ok(issued)
    }

    /// Re-shares a held grant to `new_holder`, narrowed. Works offline: the new
    /// token carries a block signed by this node. The definition is stored so
    /// the new holder can renew through us.
    pub fn reshare(&self, holder: &NodeIdentity, parent: &GrantId, new_holder: PublicKey, narrowing: Narrowing, now_ms: i64) -> Result<IssuedGrant> {
        check_ttl(narrowing.ttl_ms)?;
        match &narrowing.scopes {
            Some(scopes) if scopes.is_empty() => return Err(TrustError::NoScopes),
            Some(scopes) => scopes.iter().try_for_each(Scope::validate)?,
            None => {}
        }
        let issued = self.attenuate_from(holder, parent, GrantId::random(), new_holder, &narrowing, now_ms)?;
        self.insert_issued(&issued, &Definition::Reshare(narrowing), Some(parent), now_ms)
    }

    fn attenuate_from(&self, holder: &NodeIdentity, parent: &GrantId, id: GrantId, new_holder: PublicKey, narrowing: &Narrowing, now_ms: i64) -> Result<IssuedGrant> {
        let received = self.received(parent)?.ok_or_else(|| TrustError::UnknownGrant(parent.clone()))?;
        if !received.reshare {
            return Err(TrustError::ReshareForbidden(parent.clone()));
        }
        if received.expires_ms <= now_ms {
            return Err(TrustError::ParentExpired(parent.clone()));
        }
        let chain = Chain::parse(&received.token, &received.root).map_err(|e| TrustError::Malformed(e.to_string()))?;
        if chain.last().holder != holder.public_key() {
            return Err(TrustError::NotHolder(parent.clone()));
        }
        let expires_ms = now_ms.saturating_add(narrowing.ttl_ms).min(chain.expires_ms());
        let token = attenuate(&chain, holder, &id, &new_holder, narrowing.scopes.as_deref(), expires_ms, narrowing.reshare)?;
        Ok(IssuedGrant { id, holder: new_holder, token, expires_ms })
    }

    /// Revokes a grant this node issued: renewal is refused and this node's
    /// verifier denies its tokens at once.
    pub fn revoke(&self, issuer: &PublicKey, id: &GrantId, now_ms: i64) -> Result<()> {
        let changed = self.db.locked().execute("UPDATE grants_issued SET revoked_ms = ?2 WHERE id = ?1 AND revoked_ms IS NULL", params![id.0, now_ms])?;
        if changed > 0 {
            return self.record_revocation(id, issuer, now_ms);
        }
        let known: bool = self.db.locked().query_row("SELECT EXISTS(SELECT 1 FROM grants_issued WHERE id = ?1)", params![id.0], |r| r.get(0))?;
        if known { Ok(()) } else { Err(TrustError::UnknownGrant(id.clone())) }
    }

    /// Records a revocation notice from a peer, so this node denies re-shares of it at once.
    pub fn record_revocation(&self, id: &GrantId, issuer: &PublicKey, revoked_ms: i64) -> Result<()> {
        let sql = "INSERT OR IGNORE INTO grants_revocations (id, issuer, revoked_ms) VALUES (?1, ?2, ?3)";
        self.db.locked().execute(sql, params![id.0, issuer.to_string(), revoked_ms])?;
        Ok(())
    }

    /// Every grant this node issued, oldest first.
    pub fn issued(&self) -> Result<Vec<IssuedRecord>> {
        let db = self.db.locked();
        let mut statement = db.prepare(&format!("{ISSUED} ORDER BY created_ms, id"))?;
        let rows = statement.query_map([], issued_row)?;
        rows.map(|row| {
            let (id, holder, definition, parent, created_ms, revoked_ms) = row?;
            let (scopes, ttl_ms, reshare, until_ms) = match definition {
                Definition::Root(spec) => (Some(spec.scopes), spec.ttl_ms, spec.reshare, spec.until_ms),
                Definition::Reshare(n) => (n.scopes, n.ttl_ms, n.reshare, None),
            };
            Ok(IssuedRecord { id, holder, scopes, ttl_ms, reshare, until_ms, parent, created_ms, revoked_ms })
        })
        .collect()
    }

    /// Stores a token issued (or renewed) to `own` once its chain from `root`
    /// checks out and ends at `own`.
    pub fn receive(&self, own: &PublicKey, root: &PublicKey, token: &[u8]) -> Result<ReceivedGrant> { self.receive_checked(own, root, token, None) }
    /// [`GrantStore::receive`], accepting only a fresher token for `expected`.
    pub fn receive_renewal(&self, own: &PublicKey, root: &PublicKey, expected: &GrantId, token: &[u8]) -> Result<ReceivedGrant> {
        self.receive_checked(own, root, token, Some(expected))
    }

    fn receive_checked(&self, own: &PublicKey, root: &PublicKey, token: &[u8], expected: Option<&GrantId>) -> Result<ReceivedGrant> {
        let chain = Chain::parse(token, root).map_err(|e| TrustError::Malformed(e.to_string()))?;
        let last = chain.last();
        if last.holder != *own {
            return Err(TrustError::NotHolder(last.grant.clone()));
        }
        if let Some(id) = expected {
            if *id != last.grant {
                return Err(TrustError::Malformed("renewal returned a different grant".into()));
            }
            let old = self.received(id)?.ok_or_else(|| TrustError::UnknownGrant(id.clone()))?;
            if chain.expires_ms() <= old.expires_ms || last.issuer != old.issuer {
                return Err(TrustError::Malformed("renewal did not extend the original grant".into()));
            }
        }
        let (expires_ms, reshare) = (chain.expires_ms(), last.reshare);
        let received = ReceivedGrant { id: last.grant.clone(), issuer: last.issuer, root: *root, token: token.to_vec(), expires_ms, reshare, refused_ms: None };
        self.db.locked().execute(
            "INSERT INTO grants_received (id, issuer, root, token, expires_ms, reshare, refused_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
             ON CONFLICT(id) DO UPDATE SET token = excluded.token, expires_ms = excluded.expires_ms, reshare = excluded.reshare, refused_ms = NULL
             WHERE excluded.issuer = grants_received.issuer AND excluded.root = grants_received.root",
            params![received.id.0, received.issuer.to_string(), root.to_string(), received.token, expires_ms, reshare],
        )?;
        Ok(received)
    }

    /// Marks a held grant as refused by its issuer: renewal stops trying.
    pub fn mark_refused(&self, id: &GrantId, now_ms: i64) -> Result<()> {
        self.db.locked().execute("UPDATE grants_received SET refused_ms = ?2 WHERE id = ?1", params![id.0, now_ms])?;
        Ok(())
    }

    pub fn received(&self, id: &GrantId) -> Result<Option<ReceivedGrant>> {
        Ok(self.db.locked().query_row(&format!("{RECEIVED} WHERE id = ?1"), params![id.0], ReceivedGrant::from_row).optional()?)
    }

    /// Every grant this node holds, soonest expiry first.
    pub fn received_all(&self) -> Result<Vec<ReceivedGrant>> {
        let db = self.db.locked();
        let mut statement = db.prepare(&format!("{RECEIVED} ORDER BY expires_ms, id"))?;
        Ok(statement.query_map([], ReceivedGrant::from_row)?.collect::<rusqlite::Result<_>>()?)
    }
}

fn check_ttl(ttl_ms: i64) -> Result<()> { if ttl_ms <= 0 { Err(TrustError::Ttl(ttl_ms)) } else { Ok(()) } }

impl Revocations for GrantStore {
    /// A read failure counts as revoked: an unchecked token must not pass.
    fn is_revoked(&self, grant: &GrantId) -> bool {
        let sql = "SELECT EXISTS(SELECT 1 FROM grants_revocations WHERE id = ?1)";
        self.db.locked().query_row(sql, params![grant.0], |r| r.get(0)).unwrap_or(true)
    }
}

#[cfg(test)]
#[path = "golden.rs"]
mod golden;
