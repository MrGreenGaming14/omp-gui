//! The provider list of the sign in screen, from omp's `get_login_providers`.

use crate::Result;
use crate::rpc::{LoginProvider, OmpRpc};

/// Sign in state of one provider as the sign in screen shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignIn {
    /// omp holds a credential for the provider.
    SignedIn,
    /// A login can be started.
    SignedOut,
    /// omp offers no login for it now (for example a paused provider).
    Unavailable,
}

/// One row of the sign in screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRow {
    pub provider: LoginProvider,
    pub sign_in: SignIn,
}

impl ProviderRow {
    fn new(provider: LoginProvider) -> Self {
        let sign_in = if provider.authenticated {
            SignIn::SignedIn
        } else if provider.available {
            SignIn::SignedOut
        } else {
            SignIn::Unavailable
        };
        Self { provider, sign_in }
    }

    /// Whether the sign in button should start a login.
    pub fn can_sign_in(&self) -> bool {
        self.provider.available
    }
}

/// Orders providers for display: signed in first, then those that can sign in,
/// then unavailable ones, each group by name.
pub fn provider_rows(providers: Vec<LoginProvider>) -> Vec<ProviderRow> {
    let mut rows: Vec<ProviderRow> = providers.into_iter().map(ProviderRow::new).collect();
    rows.sort_by(|a, b| {
        let rank = |r: &ProviderRow| match r.sign_in {
            SignIn::SignedIn => 0,
            SignIn::SignedOut => 1,
            SignIn::Unavailable => 2,
        };
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.provider.name.to_lowercase().cmp(&b.provider.name.to_lowercase()))
    });
    rows
}

/// Asks omp for its login providers and returns them as display rows.
pub async fn list_providers(rpc: &dyn OmpRpc) -> Result<Vec<ProviderRow>> {
    Ok(provider_rows(rpc.get_login_providers().await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, name: &str, available: bool, authenticated: bool) -> LoginProvider {
        LoginProvider {
            id: id.into(),
            name: name.into(),
            available,
            authenticated,
        }
    }

    #[test]
    fn signed_in_providers_come_first_and_paused_ones_cannot_sign_in() {
        let rows = provider_rows(vec![
            p("google-antigravity", "Antigravity", false, false),
            p("openai-codex", "ChatGPT (Codex)", true, false),
            p("anthropic", "Anthropic (Claude Pro/Max)", true, true),
            p("github-copilot", "GitHub Copilot", true, false),
        ]);
        let ids: Vec<&str> = rows.iter().map(|r| r.provider.id.as_str()).collect();
        assert_eq!(
            ids,
            ["anthropic", "openai-codex", "github-copilot", "google-antigravity"]
        );
        assert_eq!(rows[0].sign_in, SignIn::SignedIn);
        assert_eq!(rows[3].sign_in, SignIn::Unavailable);
        assert!(!rows[3].can_sign_in());
    }
}
