//! Grant definitions in SQLite: grants issued, grants received, revocations.
//!
//! Tokens live minutes. The issuer keeps each grant's definition and mints a
//! fresh token whenever the holder asks while the grant is active. Revoking
//! marks the definition, so renewal stops and the last token runs out.

use crate::key::{NodeIdentity, PublicKey};
use crate::scope::Scope;
use crate::token::{Chain, GrantId, Revocations, attenuate, mint};
use crate::{Result, TrustError};
use loa_util::LockExt;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS grants_issued (
    id TEXT PRIMARY KEY, holder TEXT NOT NULL, spec TEXT NOT NULL, parent TEXT,
    created_ms INTEGER NOT NULL, revoked_ms INTEGER
);
CREATE TABLE IF NOT EXISTS grants_received (
    id TEXT PRIMARY KEY, issuer TEXT NOT NULL, root TEXT NOT NULL, token BLOB NOT NULL,
    expires_ms INTEGER NOT NULL, reshare INTEGER NOT NULL, refused_ms INTEGER
);
CREATE TABLE IF NOT EXISTS grants_revocations (
    id TEXT PRIMARY KEY, issuer TEXT NOT NULL, revoked_ms INTEGER NOT NULL
);
";

const ISSUED_COLUMNS: &str = "SELECT id, holder, spec, parent, created_ms, revoked_ms FROM grants_issued";
const RECEIVED_COLUMNS: &str = "SELECT id, issuer, root, token, expires_ms, reshare, refused_ms FROM grants_received";

/// What a new grant gives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantSpec {
    pub scopes: Vec<Scope>,
    /// Lifetime of each token. Renewal hands out a new one.
    pub ttl_ms: i64,
    /// Whether the holder may re-share a narrower grant.
    pub reshare: bool,
    /// When the grant ends: no token outlives it and renewal stops. `None`
    /// renews until revoked.
    #[serde(default)]
    pub until_ms: Option<i64>,
}

impl GrantSpec {
    fn token_expiry(&self, id: &GrantId, now_ms: i64) -> Result<i64> {
        let expires_ms = now_ms.saturating_add(self.ttl_ms);
        match self.until_ms {
            Some(until) if until <= now_ms => Err(TrustError::Expired(id.clone())),
            Some(until) => Ok(expires_ms.min(until)),
            None => Ok(expires_ms),
        }
    }
}

/// How a holder narrows a grant it re-shares. Rights are the intersection of
/// the parent's and these, so a re-share can never widen.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Narrowing {
    /// Restrict to these scopes; `None` keeps the parent's.
    pub scopes: Option<Vec<Scope>>,
    /// Lifetime of each token, capped by the parent token's expiry.
    pub ttl_ms: i64,
    /// Whether the new holder may re-share in turn.
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
    /// Who renews it: the root node, or the peer that re-shared it.
    pub issuer: PublicKey,
    /// The node whose resources it covers and that verifies it.
    pub root: PublicKey,
    pub token: Vec<u8>,
    pub expires_ms: i64,
    /// Whether this node may re-share it.
    pub reshare: bool,
    /// When the issuer refused to renew it (revoked).
    pub refused_ms: Option<i64>,
}

impl ReceivedGrant {
    /// The rights its root signed into it; see [`crate::token_scopes`].
    pub fn scopes(&self) -> Result<Vec<Scope>> {
        crate::token::token_scopes(&self.token, &self.root)
    }

    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: GrantId(r.get(0)?),
            issuer: key_column(r, 1)?,
            root: key_column(r, 2)?,
            token: r.get(3)?,
            expires_ms: r.get(4)?,
            reshare: r.get(5)?,
            refused_ms: r.get(6)?,
        })
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
    /// When a root grant ends; `None` renews until revoked (and for re-shares,
    /// which end with their parent).
    pub until_ms: Option<i64>,
    /// The received grant a re-share was cut from.
    pub parent: Option<GrantId>,
    pub created_ms: i64,
    pub revoked_ms: Option<i64>,
}

/// One issued row with its definition still typed.
struct IssuedRow {
    holder: PublicKey,
    definition: Definition,
    parent: Option<GrantId>,
    created_ms: i64,
    revoked_ms: Option<i64>,
}

impl IssuedRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<(GrantId, Self)> {
        let spec: String = r.get(2)?;
        let definition = serde_json::from_str(&spec).map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, e.into()))?;
        let row = Self { holder: key_column(r, 1)?, definition, parent: r.get::<_, Option<String>>(3)?.map(GrantId), created_ms: r.get(4)?, revoked_ms: r.get(5)? };
        Ok((GrantId(r.get(0)?), row))
    }
}

fn key_column(r: &Row<'_>, i: usize) -> rusqlite::Result<PublicKey> {
    PublicKey::from_hex(&r.get::<_, String>(i)?).map_err(|e| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, e.into()))
}

/// One node's grant tables.
pub struct GrantStore {
    db: Mutex<Connection>,
}

impl std::fmt::Debug for GrantStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrantStore").finish_non_exhaustive()
    }
}

impl GrantStore {
    pub fn open(path: &Path) -> Result<Self> {
        Self::with_connection(Connection::open(path)?)
    }

    /// Uses an open connection, creating the tables if needed.
    pub fn with_connection(db: Connection) -> Result<Self> {
        db.execute_batch(SCHEMA)?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn insert_issued(&self, id: &GrantId, holder: &PublicKey, definition: &Definition, parent: Option<&GrantId>, now_ms: i64) -> Result<()> {
        self.db.locked().execute(
            "INSERT INTO grants_issued (id, holder, spec, parent, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id.0, holder.to_string(), serde_json::to_string(definition)?, parent.map(|p| &p.0), now_ms],
        )?;
        Ok(())
    }

    /// The active (not revoked) definition of a grant this node issued.
    fn active(&self, id: &GrantId) -> Result<IssuedRow> {
        let row = self.db.locked().query_row(&format!("{ISSUED_COLUMNS} WHERE id = ?1"), params![id.0], IssuedRow::from_row).optional()?;
        let (_, row) = row.ok_or_else(|| TrustError::UnknownGrant(id.clone()))?;
        if row.revoked_ms.is_some() {
            return Err(TrustError::Revoked(id.clone()));
        }
        Ok(row)
    }

    fn mint_root(issuer: &NodeIdentity, id: &GrantId, holder: PublicKey, spec: &GrantSpec, now_ms: i64) -> Result<IssuedGrant> {
        for scope in &spec.scopes {
            scope.validate()?;
        }
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
        self.insert_issued(&issued.id, &holder, &Definition::Root(spec), None, now_ms)?;
        Ok(issued)
    }

    /// Mints a fresh token for an active grant this node issued. Only its
    /// holder may ask; a revoked grant is refused.
    pub fn renew(&self, issuer: &NodeIdentity, id: &GrantId, requester: &PublicKey, now_ms: i64) -> Result<IssuedGrant> {
        let row = self.active(id)?;
        if row.holder != *requester {
            return Err(TrustError::NotHolder(id.clone()));
        }
        match (row.definition, row.parent) {
            (Definition::Root(spec), None) => Self::mint_root(issuer, id, row.holder, &spec, now_ms),
            (Definition::Reshare(narrowing), Some(parent)) => self.attenuate_from(issuer, &parent, id.clone(), row.holder, &narrowing, now_ms),
            _ => Err(TrustError::Malformed(format!("grant {id} has a parent only when it is a re-share"))),
        }
    }

    /// Moves the end of a root grant this node issued to `until_ms` (`None`:
    /// until revoked) and mints a token right away, so the holder sees the
    /// change without waiting for its next renewal.
    pub fn extend(&self, issuer: &NodeIdentity, id: &GrantId, until_ms: Option<i64>, now_ms: i64) -> Result<IssuedGrant> {
        let row = self.active(id)?;
        let (Definition::Root(mut spec), None) = (row.definition, row.parent) else {
            return Err(TrustError::Malformed(format!("only root grants can be extended, {id} is a re-share")));
        };
        spec.until_ms = until_ms;
        let issued = Self::mint_root(issuer, id, row.holder, &spec, now_ms)?;
        self.db.locked().execute("UPDATE grants_issued SET spec = ?2 WHERE id = ?1", params![id.0, serde_json::to_string(&Definition::Root(spec))?])?;
        Ok(issued)
    }

    /// Re-shares a grant this node holds to `new_holder`, narrowed. Works
    /// offline: the new token carries a block signed by this node's key. The
    /// definition is stored so the new holder can renew through us.
    pub fn reshare(&self, holder: &NodeIdentity, parent: &GrantId, new_holder: PublicKey, narrowing: Narrowing, now_ms: i64) -> Result<IssuedGrant> {
        check_ttl(narrowing.ttl_ms)?;
        if let Some(scopes) = &narrowing.scopes {
            if scopes.is_empty() {
                return Err(TrustError::NoScopes);
            }
            scopes.iter().try_for_each(Scope::validate)?;
        }
        let issued = self.attenuate_from(holder, parent, GrantId::random(), new_holder, &narrowing, now_ms)?;
        self.insert_issued(&issued.id, &new_holder, &Definition::Reshare(narrowing), Some(parent), now_ms)?;
        Ok(issued)
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

    /// Revokes a grant this node issued: renewal is refused from now on and
    /// this node's verifier denies its tokens at once.
    pub fn revoke(&self, issuer: &PublicKey, id: &GrantId, now_ms: i64) -> Result<()> {
        let changed = {
            let db = self.db.locked();
            let changed = db.execute("UPDATE grants_issued SET revoked_ms = ?2 WHERE id = ?1 AND revoked_ms IS NULL", params![id.0, now_ms])?;
            let known: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM grants_issued WHERE id = ?1)", params![id.0], |r| r.get(0))?;
            if !known {
                return Err(TrustError::UnknownGrant(id.clone()));
            }
            changed
        };
        if changed > 0 {
            self.record_revocation(id, issuer, now_ms)?;
        }
        Ok(())
    }

    /// Records a revocation notice from a peer, so this node's verifier denies
    /// re-shares of that grant at once.
    pub fn record_revocation(&self, id: &GrantId, issuer: &PublicKey, revoked_ms: i64) -> Result<()> {
        self.db.locked().execute(
            "INSERT OR IGNORE INTO grants_revocations (id, issuer, revoked_ms) VALUES (?1, ?2, ?3)",
            params![id.0, issuer.to_string(), revoked_ms],
        )?;
        Ok(())
    }

    /// Every grant this node issued, oldest first.
    pub fn issued(&self) -> Result<Vec<IssuedRecord>> {
        let db = self.db.locked();
        let mut statement = db.prepare(&format!("{ISSUED_COLUMNS} ORDER BY created_ms, id"))?;
        let rows = statement.query_map([], IssuedRow::from_row)?;
        rows.map(|row| {
            let (id, row) = row?;
            let (scopes, ttl_ms, reshare, until_ms) = match row.definition {
                Definition::Root(spec) => (Some(spec.scopes), spec.ttl_ms, spec.reshare, spec.until_ms),
                Definition::Reshare(n) => (n.scopes, n.ttl_ms, n.reshare, None),
            };
            Ok(IssuedRecord { id, holder: row.holder, scopes, ttl_ms, reshare, until_ms, parent: row.parent, created_ms: row.created_ms, revoked_ms: row.revoked_ms })
        })
        .collect()
    }

    /// Stores a token issued (or renewed) to `own`. Checks it is signed by
    /// `root`, its chain is sound and its last holder is `own`.
    pub fn receive(&self, own: &PublicKey, root: &PublicKey, token: &[u8]) -> Result<ReceivedGrant> {
        self.receive_checked(own, root, token, None)
    }

    /// Accepts only a fresher token for the grant being renewed.
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
        let received = ReceivedGrant {
            id: last.grant.clone(),
            issuer: last.issuer,
            root: *root,
            token: token.to_vec(),
            expires_ms: chain.expires_ms(),
            reshare: last.reshare,
            refused_ms: None,
        };
        self.db.locked().execute(
            "INSERT INTO grants_received (id, issuer, root, token, expires_ms, reshare, refused_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)
             ON CONFLICT(id) DO UPDATE SET token = excluded.token, expires_ms = excluded.expires_ms,
               reshare = excluded.reshare, refused_ms = NULL
             WHERE excluded.issuer = grants_received.issuer AND excluded.root = grants_received.root",
            params![received.id.0, received.issuer.to_string(), root.to_string(), received.token, received.expires_ms, received.reshare],
        )?;
        Ok(received)
    }

    /// Marks a held grant as refused by its issuer: renewal stops trying.
    pub fn mark_refused(&self, id: &GrantId, now_ms: i64) -> Result<()> {
        self.db.locked().execute("UPDATE grants_received SET refused_ms = ?2 WHERE id = ?1", params![id.0, now_ms])?;
        Ok(())
    }

    pub fn received(&self, id: &GrantId) -> Result<Option<ReceivedGrant>> {
        let db = self.db.locked();
        Ok(db.query_row(&format!("{RECEIVED_COLUMNS} WHERE id = ?1"), params![id.0], ReceivedGrant::from_row).optional()?)
    }

    /// Every grant this node holds, soonest expiry first.
    pub fn received_all(&self) -> Result<Vec<ReceivedGrant>> {
        let db = self.db.locked();
        let mut statement = db.prepare(&format!("{RECEIVED_COLUMNS} ORDER BY expires_ms, id"))?;
        Ok(statement.query_map([], ReceivedGrant::from_row)?.collect::<rusqlite::Result<_>>()?)
    }
}

fn check_ttl(ttl_ms: i64) -> Result<()> {
    if ttl_ms <= 0 { Err(TrustError::Ttl(ttl_ms)) } else { Ok(()) }
}

impl Revocations for GrantStore {
    fn is_revoked(&self, grant: &GrantId) -> bool {
        // A read failure denies: a verifier that can't check revocations must
        // not let a possibly revoked token through.
        self.db
            .locked()
            .query_row("SELECT EXISTS(SELECT 1 FROM grants_revocations WHERE id = ?1)", params![grant.0], |r| r.get(0))
            .unwrap_or(true)
    }
}
