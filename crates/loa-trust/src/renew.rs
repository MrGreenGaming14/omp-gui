//! Holder side renewal: refresh short lived tokens before they run out.

use crate::key::PublicKey;
use crate::store::{GrantStore, ReceivedGrant};
use crate::token::GrantId;
use crate::{Result, TrustError};

/// Why an issuer didn't hand out a fresh token.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RenewFailure {
    /// Revoked, unknown, ended or not ours. Final.
    #[error("the issuer refused to renew: {0}")]
    Refused(String),
    /// Unreachable or failed. Retried next round.
    #[error("renewal failed: {0}")]
    Unreachable(String),
}

impl RenewFailure {
    /// How the issuer side reports a failed [`GrantStore::renew`].
    pub fn from_issuer(error: &TrustError) -> Self {
        use TrustError::*;
        match error {
            Revoked(_) | Expired(_) | UnknownGrant(_) | NotHolder(_) | ReshareForbidden(_) => RenewFailure::Refused(error.to_string()),
            other => RenewFailure::Unreachable(other.to_string()),
        }
    }
}

/// Asks a grant's issuer for a fresh token (over `omp/control/1`).
pub trait RenewTransport {
    fn renew(&self, issuer: PublicKey, grant: GrantId) -> impl Future<Output = Result<Vec<u8>, RenewFailure>> + Send;
}

/// What happened to one grant in a renewal round.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenewOutcome {
    Renewed { grant: GrantId, expires_ms: i64 },
    /// The issuer refused; the grant is marked and not asked again.
    Refused { grant: GrantId, reason: String },
    /// Try again next round.
    Failed { grant: GrantId, reason: String },
}

/// Keeps held grants fresh: call [`Renewer::renew_due`] at [`Renewer::next_due`].
#[derive(Clone, Debug)]
pub struct Renewer {
    own: PublicKey,
    /// Renew this long before a token expires.
    margin_ms: i64,
}

impl Renewer {
    pub fn new(own: PublicKey, margin_ms: i64) -> Self { Self { own, margin_ms } }
    fn due_at(&self, grant: &ReceivedGrant) -> Option<i64> { grant.refused_ms.is_none().then(|| grant.expires_ms.saturating_sub(self.margin_ms)) }

    /// When the next renewal is due; `None` when nothing needs renewing.
    pub fn next_due(&self, store: &GrantStore) -> Result<Option<i64>> { Ok(store.received_all()?.iter().filter_map(|g| self.due_at(g)).min()) }
    /// Renews every held grant due at `now_ms` through `transport`.
    pub async fn renew_due<T: RenewTransport>(&self, store: &GrantStore, transport: &T, now_ms: i64) -> Result<Vec<RenewOutcome>> {
        let mut outcomes = Vec::new();
        for grant in store.received_all()?.into_iter().filter(|g| self.due_at(g).is_some_and(|due| due <= now_ms)) {
            let id = grant.id.clone();
            outcomes.push(match transport.renew(grant.issuer, grant.id).await {
                Ok(token) => match store.receive_renewal(&self.own, &grant.root, &id, &token) {
                    Ok(fresh) => RenewOutcome::Renewed { grant: id, expires_ms: fresh.expires_ms },
                    Err(e) => RenewOutcome::Failed { grant: id, reason: e.to_string() },
                },
                Err(RenewFailure::Refused(reason)) => {
                    store.mark_refused(&id, now_ms)?;
                    RenewOutcome::Refused { grant: id, reason }
                }
                Err(RenewFailure::Unreachable(reason)) => RenewOutcome::Failed { grant: id, reason },
            });
        }
        Ok(outcomes)
    }
}
