//! Holder side renewal: refresh short lived tokens before they run out.

use crate::keys::NodePublicKey;
use crate::store::GrantStore;
use crate::token::GrantId;
use crate::{GrantError, Result};

/// Why an issuer didn't hand out a fresh token.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RenewFailure {
    /// The issuer refuses: the grant is revoked, unknown, or not ours. Final.
    #[error("the issuer refused to renew: {0}")]
    Refused(String),
    /// The issuer couldn't be reached or failed. Retried next round.
    #[error("renewal failed: {0}")]
    Unreachable(String),
}

impl RenewFailure {
    /// How the issuer side reports a failed [`GrantStore::renew`].
    pub fn from_issuer(error: &GrantError) -> Self {
        match error {
            GrantError::Revoked(_)
            | GrantError::UnknownGrant(_)
            | GrantError::NotHolder(_)
            | GrantError::ReshareForbidden(_) => RenewFailure::Refused(error.to_string()),
            other => RenewFailure::Unreachable(other.to_string()),
        }
    }
}

/// Asks a grant's issuer for a fresh token (over `omp/control/1`). Port:
/// `loa-net` implements it in the app.
pub trait RenewTransport {
    fn renew(
        &self,
        issuer: NodePublicKey,
        grant: GrantId,
    ) -> impl Future<Output = Result<Vec<u8>, RenewFailure>> + Send;
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

/// Keeps the grants a node holds fresh. The app calls
/// [`Renewer::renew_due`] when [`Renewer::next_due`] comes around.
#[derive(Clone, Debug)]
pub struct Renewer {
    own: NodePublicKey,
    /// Renew this long before a token expires.
    margin_ms: i64,
}

impl Renewer {
    pub fn new(own: NodePublicKey, margin_ms: i64) -> Self {
        Self { own, margin_ms }
    }

    /// When the next renewal is due, or `None` when nothing needs renewing.
    pub fn next_due(&self, store: &GrantStore) -> Result<Option<i64>> {
        Ok(store
            .received_all()?
            .iter()
            .filter(|g| g.refused_ms.is_none())
            .map(|g| g.expires_ms.saturating_sub(self.margin_ms))
            .min())
    }

    /// Renews every held grant due at `now_ms` through `transport`.
    pub async fn renew_due<T: RenewTransport>(
        &self,
        store: &GrantStore,
        transport: &T,
        now_ms: i64,
    ) -> Result<Vec<RenewOutcome>> {
        let due: Vec<_> = store
            .received_all()?
            .into_iter()
            .filter(|g| g.refused_ms.is_none() && g.expires_ms.saturating_sub(self.margin_ms) <= now_ms)
            .collect();
        let mut outcomes = Vec::with_capacity(due.len());
        for grant in due {
            let outcome = match transport.renew(grant.issuer, grant.id.clone()).await {
                Ok(token) => match store.receive_renewal(&self.own, &grant.root, &grant.id, &token) {
                    Ok(fresh) => RenewOutcome::Renewed { grant: grant.id, expires_ms: fresh.expires_ms },
                    Err(e) => RenewOutcome::Failed { grant: grant.id, reason: e.to_string() },
                },
                Err(RenewFailure::Refused(reason)) => {
                    store.mark_refused(&grant.id, now_ms)?;
                    RenewOutcome::Refused { grant: grant.id, reason }
                }
                Err(RenewFailure::Unreachable(reason)) => RenewOutcome::Failed { grant: grant.id, reason },
            };
            outcomes.push(outcome);
        }
        Ok(outcomes)
    }
}
