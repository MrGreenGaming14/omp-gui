//! Shared process, configuration and login support for harness accounts.

pub mod cli;
pub mod configdoc;
pub mod opener;
pub mod process;
use loa_settings::models::error::{Error, Result};

pub use loa_contracts::harness::{LoginKind, SignInMethod, SignInState};

/// Checks a model name before it is written to a harness config: not empty and
/// free of whitespace and control characters.
pub(crate) fn validate_model_name(field: &str, name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::invalid(field, "model name is empty"));
    }
    if name.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Error::invalid(
            field,
            "model name contains whitespace or control characters",
        ));
    }
    Ok(())
}
