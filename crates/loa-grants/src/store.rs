//! Grant definitions in SQLite: grants issued, grants received, revocations.
//!
//! Tokens live minutes. The issuer keeps each grant's definition and mints a
//! fresh token whenever the holder asks while the grant is active. Revoking
//! marks the definition, so renewal stops and the last token runs out.

use loa_util::LockExt;
use crate::keys::{NodeKey, NodePublicKey};
use crate::scope::Scope;
use crate::token::{Chain, GrantId, Revocations, attenuate, mint};
use crate::{GrantError, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS grants_issued (
    id TEXT PRIMARY KEY,
    holder TEXT NOT NULL,
    spec TEXT NOT NULL,
    parent TEXT,
    created_ms INTEGER NOT NULL,
    revoked_ms INTEGER
);
CREATE TABLE IF NOT EXISTS grants_received (
    id TEXT PRIMARY KEY,
    issuer TEXT NOT NULL,
    root TEXT NOT NULL,
    token BLOB NOT NULL,
    expires_ms INTEGER NOT NULL,
    reshare INTEGER NOT NULL,
    refused_ms INTEGER
);
CREATE TABLE IF NOT EXISTS grants_revocations (
    id TEXT PRIMARY KEY,
    issuer TEXT NOT NULL,
    revoked_ms INTEGER NOT NULL
);
";

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
            Some(until) if until <= now_ms => Err(GrantError::Expired(id.clone())),
            Some(until) => Ok(expires_ms.min(until)),
            None => Ok(expires_ms),
        }
    }
}

/// How a holder narrows a grant it re-shares. Rights are the intersection
/// of the parent's and these, so a re-share can never widen.
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
    pub holder: NodePublicKey,
    pub token: Vec<u8>,
    pub expires_ms: i64,
}

/// A grant this node holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceivedGrant {
    pub id: GrantId,
    /// Who renews it: the root node, or the peer that re-shared it.
    pub issuer: NodePublicKey,
    /// The node whose resources it covers and that verifies it.
    pub root: NodePublicKey,
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
}

/// A grant this node issued, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssuedRecord {
    pub id: GrantId,
    pub holder: NodePublicKey,
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
    /// Opens (or creates) the tables in the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        Self::with_connection(Connection::open(path)?)
    }

    /// Uses an open connection, creating the tables if needed.
    pub fn with_connection(db: Connection) -> Result<Self> {
        db.execute_batch(SCHEMA)?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// Creates a grant for `holder` and mints its first token.
    pub fn issue(&self, issuer: &NodeKey, holder: NodePublicKey, spec: GrantSpec, now_ms: i64) -> Result<IssuedGrant> {
        if spec.scopes.is_empty() {
            return Err(GrantError::NoScopes);
        }
        for scope in &spec.scopes {
            scope.validate()?;
        }
        check_ttl(spec.ttl_ms)?;
        let id = GrantId::random()?;
        let expires_ms = spec.token_expiry(&id, now_ms)?;
        let token = mint(issuer, &id, &holder, &spec.scopes, expires_ms, spec.reshare)?;
        let definition = serde_json::to_string(&Definition::Root(spec))?;
        self.db.locked().execute(
            "INSERT INTO grants_issued (id, holder, spec, parent, created_ms) VALUES (?1, ?2, ?3, NULL, ?4)",
            params![id.0, holder.to_hex(), definition, now_ms],
        )?;
        Ok(IssuedGrant { id, holder, token, expires_ms })
    }

    /// Mints a fresh token for an active grant this node issued. Only its
    /// holder may ask; a revoked grant is refused.
    pub fn renew(&self, issuer: &NodeKey, id: &GrantId, requester: &NodePublicKey, now_ms: i64) -> Result<IssuedGrant> {
        let row = self
            .db.locked()
            .query_row(
                "SELECT holder, spec, parent, revoked_ms FROM grants_issued WHERE id = ?1",
                params![id.0],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<i64>>(3)?)),
            )
            .optional()?;
        let Some((holder, definition, parent, revoked)) = row else {
            return Err(GrantError::UnknownGrant(id.clone()));
        };
        if revoked.is_some() {
            return Err(GrantError::Revoked(id.clone()));
        }
        let holder = NodePublicKey::from_hex(&holder)?;
        if holder != *requester {
            return Err(GrantError::NotHolder(id.clone()));
        }
        let definition: Definition = serde_json::from_str(&definition)?;
        match (definition, parent) {
            (Definition::Root(spec), None) => {
                for scope in &spec.scopes {
                    scope.validate()?;
                }
                let expires_ms = spec.token_expiry(id, now_ms)?;
                let token = mint(issuer, id, &holder, &spec.scopes, expires_ms, spec.reshare)?;
                Ok(IssuedGrant { id: id.clone(), holder, token, expires_ms })
            }
            (Definition::Root(_), Some(_)) => Err(GrantError::Malformed(format!("root grant {id} has a parent"))),
            (Definition::Reshare(narrowing), Some(parent)) => {
                self.attenuate_from(issuer, &GrantId(parent), id.clone(), holder, &narrowing, now_ms)
            }
            (Definition::Reshare(_), None) => Err(GrantError::Malformed(format!("re-share {id} has no parent"))),
        }
    }

    /// Moves the end of a root grant this node issued to `until_ms` (`None`:
    /// renews until revoked) and mints a token for its holder right away, so
    /// the change reaches the holder without waiting for its next renewal.
    pub fn extend(&self, issuer: &NodeKey, id: &GrantId, until_ms: Option<i64>, now_ms: i64) -> Result<IssuedGrant> {
        let row = self
            .db.locked()
            .query_row(
                "SELECT holder, spec, parent, revoked_ms FROM grants_issued WHERE id = ?1",
                params![id.0],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, Option<i64>>(3)?)),
            )
            .optional()?;
        let Some((holder, definition, parent, revoked)) = row else {
            return Err(GrantError::UnknownGrant(id.clone()));
        };
        if revoked.is_some() {
            return Err(GrantError::Revoked(id.clone()));
        }
        let (Definition::Root(mut spec), None) = (serde_json::from_str(&definition)?, parent) else {
            return Err(GrantError::Malformed(format!("only root grants can be extended, {id} is a re-share")));
        };
        spec.until_ms = until_ms;
        let holder = NodePublicKey::from_hex(&holder)?;
        let expires_ms = spec.token_expiry(id, now_ms)?;
        let token = mint(issuer, id, &holder, &spec.scopes, expires_ms, spec.reshare)?;
        self.db.locked().execute(
            "UPDATE grants_issued SET spec = ?2 WHERE id = ?1",
            params![id.0, serde_json::to_string(&Definition::Root(spec))?],
        )?;
        Ok(IssuedGrant { id: id.clone(), holder, token, expires_ms })
    }

    /// Re-shares a grant this node holds to `new_holder`, narrowed. Works
    /// offline: the new token carries a block signed by this node's key.
    /// The definition is stored so the new holder can renew through us.
    pub fn reshare(
        &self,
        holder: &NodeKey,
        parent: &GrantId,
        new_holder: NodePublicKey,
        narrowing: Narrowing,
        now_ms: i64,
    ) -> Result<IssuedGrant> {
        check_ttl(narrowing.ttl_ms)?;
        if narrowing.scopes.as_ref().is_some_and(Vec::is_empty) {
            return Err(GrantError::NoScopes);
        }
        if let Some(scopes) = &narrowing.scopes {
            for scope in scopes {
                scope.validate()?;
            }
        }
        let id = GrantId::random()?;
        let issued = self.attenuate_from(holder, parent, id, new_holder, &narrowing, now_ms)?;
        let definition = serde_json::to_string(&Definition::Reshare(narrowing))?;
        self.db.locked().execute(
            "INSERT INTO grants_issued (id, holder, spec, parent, created_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![issued.id.0, new_holder.to_hex(), definition, parent.0, now_ms],
        )?;
        Ok(issued)
    }

    fn attenuate_from(
        &self,
        holder: &NodeKey,
        parent: &GrantId,
        id: GrantId,
        new_holder: NodePublicKey,
        narrowing: &Narrowing,
        now_ms: i64,
    ) -> Result<IssuedGrant> {
        let received = self.received(parent)?.ok_or_else(|| GrantError::UnknownGrant(parent.clone()))?;
        if !received.reshare {
            return Err(GrantError::ReshareForbidden(parent.clone()));
        }
        if received.expires_ms <= now_ms {
            return Err(GrantError::ParentExpired(parent.clone()));
        }
        let chain = Chain::parse(&received.token, &received.root).map_err(|e| GrantError::Malformed(e.to_string()))?;
        if chain.last().holder != holder.public() {
            return Err(GrantError::NotHolder(parent.clone()));
        }
        let expires_ms = now_ms.saturating_add(narrowing.ttl_ms).min(chain.expires_ms());
        let token =
            attenuate(&chain, holder, &id, &new_holder, narrowing.scopes.as_deref(), expires_ms, narrowing.reshare)?;
        Ok(IssuedGrant { id, holder: new_holder, token, expires_ms })
    }

    /// Revokes a grant this node issued: renewal is refused from now on and
    /// this node's verifier denies its tokens at once.
    pub fn revoke(&self, issuer: &NodePublicKey, id: &GrantId, now_ms: i64) -> Result<()> {
        let db = self.db.locked();
        let changed = db.execute(
            "UPDATE grants_issued SET revoked_ms = ?2 WHERE id = ?1 AND revoked_ms IS NULL",
            params![id.0, now_ms],
        )?;
        let known: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM grants_issued WHERE id = ?1)", params![id.0], |r| r.get(0))?;
        if !known {
            return Err(GrantError::UnknownGrant(id.clone()));
        }
        if changed > 0 {
            db.execute(
                "INSERT OR IGNORE INTO grants_revocations (id, issuer, revoked_ms) VALUES (?1, ?2, ?3)",
                params![id.0, issuer.to_hex(), now_ms],
            )?;
        }
        Ok(())
    }

    /// Records a revocation notice from a peer (received on the control
    /// ALPN), so this node's verifier denies re-shares of that grant at once.
    pub fn record_revocation(&self, id: &GrantId, issuer: &NodePublicKey, revoked_ms: i64) -> Result<()> {
        self.db.locked().execute(
            "INSERT OR IGNORE INTO grants_revocations (id, issuer, revoked_ms) VALUES (?1, ?2, ?3)",
            params![id.0, issuer.to_hex(), revoked_ms],
        )?;
        Ok(())
    }

    /// Every grant this node issued, oldest first.
    pub fn issued(&self) -> Result<Vec<IssuedRecord>> {
        let db = self.db.locked();
        let mut statement =
            db.prepare("SELECT id, holder, spec, parent, created_ms, revoked_ms FROM grants_issued ORDER BY created_ms, id")?;
        let rows = statement.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, Option<i64>>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, holder, definition, parent, created_ms, revoked_ms) = row?;
            let (scopes, ttl_ms, reshare, until_ms) = match serde_json::from_str(&definition)? {
                Definition::Root(spec) => (Some(spec.scopes), spec.ttl_ms, spec.reshare, spec.until_ms),
                Definition::Reshare(narrowing) => (narrowing.scopes, narrowing.ttl_ms, narrowing.reshare, None),
            };
            out.push(IssuedRecord {
                id: GrantId(id),
                holder: NodePublicKey::from_hex(&holder)?,
                scopes,
                ttl_ms,
                reshare,
                until_ms,
                parent: parent.map(GrantId),
                created_ms,
                revoked_ms,
            });
        }
        Ok(out)
    }

    /// Stores a token issued (or renewed) to `own`. Checks it is signed by
    /// `root`, its chain is sound and its last holder is `own`.
    pub fn receive(&self, own: &NodePublicKey, root: &NodePublicKey, token: &[u8]) -> Result<ReceivedGrant> {
        self.receive_checked(own, root, token, None)
    }

    /// Accepts only a fresher token for the grant being renewed, before
    /// writing it to the received-grants table.
    pub fn receive_renewal(
        &self,
        own: &NodePublicKey,
        root: &NodePublicKey,
        expected: &GrantId,
        token: &[u8],
    ) -> Result<ReceivedGrant> {
        self.receive_checked(own, root, token, Some(expected))
    }

    fn receive_checked(
        &self,
        own: &NodePublicKey,
        root: &NodePublicKey,
        token: &[u8],
        expected: Option<&GrantId>,
    ) -> Result<ReceivedGrant> {
        let chain = Chain::parse(token, root).map_err(|e| GrantError::Malformed(e.to_string()))?;
        let last = chain.last();
        if last.holder != *own {
            return Err(GrantError::NotHolder(last.grant.clone()));
        }
        if expected.is_some_and(|id| *id != last.grant) {
            return Err(GrantError::Malformed("renewal returned a different grant".into()));
        }
        if let Some(id) = expected {
            let old = self.received(id)?.ok_or_else(|| GrantError::UnknownGrant(id.clone()))?;
            if chain.expires_ms() <= old.expires_ms || last.issuer != old.issuer {
                return Err(GrantError::Malformed("renewal did not extend the original grant".into()));
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
            params![received.id.0, received.issuer.to_hex(), root.to_hex(), received.token, received.expires_ms, received.reshare],
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
        let row = db
            .query_row(
                "SELECT id, issuer, root, token, expires_ms, reshare, refused_ms FROM grants_received WHERE id = ?1",
                params![id.0],
                received_row,
            )
            .optional()?;
        row.map(decode_received).transpose()
    }

    /// Every grant this node holds, soonest expiry first.
    pub fn received_all(&self) -> Result<Vec<ReceivedGrant>> {
        let db = self.db.locked();
        let mut statement = db.prepare(
            "SELECT id, issuer, root, token, expires_ms, reshare, refused_ms FROM grants_received ORDER BY expires_ms, id",
        )?;
        let rows = statement.query_map([], received_row)?;
        rows.map(|row| decode_received(row?)).collect()
    }
}

type ReceivedRow = (String, String, String, Vec<u8>, i64, bool, Option<i64>);

fn received_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReceivedRow> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
}

fn decode_received((id, issuer, root, token, expires_ms, reshare, refused_ms): ReceivedRow) -> Result<ReceivedGrant> {
    Ok(ReceivedGrant {
        id: GrantId(id),
        issuer: NodePublicKey::from_hex(&issuer)?,
        root: NodePublicKey::from_hex(&root)?,
        token,
        expires_ms,
        reshare,
        refused_ms,
    })
}

fn check_ttl(ttl_ms: i64) -> Result<()> {
    if ttl_ms <= 0 {
        return Err(GrantError::Ttl(ttl_ms));
    }
    Ok(())
}

impl Revocations for GrantStore {
    fn is_revoked(&self, grant: &GrantId) -> bool {
        // A read failure denies: a verifier that can't check revocations
        // must not let a possibly revoked token through.
        self.db.locked()
            .query_row("SELECT EXISTS(SELECT 1 FROM grants_revocations WHERE id = ?1)", params![grant.0], |r| r.get(0))
            .unwrap_or(true)
    }
}
