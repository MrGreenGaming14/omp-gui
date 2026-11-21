//! Switches a product's active version between turns, keeping the previous one for rollback.
//! `<root>/versions/<product>/active.json` is replaced atomically.

pub use crate::model::SwapOutcome;
use loa_settings::updates::Active;
use crate::signature;
use crate::stage::{self, Staged};
use crate::{Error, Result};
use ed25519_dalek::VerifyingKey;
use semver::Version;
use std::path::PathBuf;

/// Whether a turn runs; swaps only happen when none does.
pub trait Activity: Send + Sync {
    fn turn_running(&self) -> bool;
}

/// Swaps and rolls back versions installed under `root`.
#[derive(Clone, Debug)]
pub struct Swapper {
    pub root: PathBuf,
    /// Versions older than the previous one kept after a swap.
    pub keep: usize,
    /// The product whose versions must always carry a trusted signature.
    pub app_product: String,
}

impl Swapper {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), keep: 0, app_product: "app".to_owned() }
    }

    pub fn active(&self, product: &str) -> Result<Option<Active>> {
        crate::read_json(&self.active_path(product))
    }

    /// Makes `to` current unless a turn runs; `trusted` keys are required for the app and,
    /// when given, for any product. Versions older than the previous one are pruned to `keep`.
    pub fn try_swap(&self, product: &str, to: &Version, trusted: Option<&[VerifyingKey]>, activity: &dyn Activity) -> Result<SwapOutcome> {
        if activity.turn_running() {
            return Ok(SwapOutcome::Deferred);
        }
        let old = self.active(product)?;
        if old.as_ref().is_some_and(|a| a.current == *to) {
            return Ok(SwapOutcome::AlreadyActive);
        }
        self.verified(product, to, trusted)?;
        let from = old.map(|a| a.current);
        let active = Active { current: to.clone(), previous: from.clone() };
        crate::write_json(&self.active_path(product), &active)?;
        self.prune(product, &active);
        Ok(SwapOutcome::Swapped { from, to: to.clone() })
    }

    /// Makes the previous version current again, keeping the one rolled back from as previous.
    pub fn rollback(&self, product: &str, trusted: Option<&[VerifyingKey]>, activity: &dyn Activity) -> Result<SwapOutcome> {
        if activity.turn_running() {
            return Ok(SwapOutcome::Deferred);
        }
        let no_previous = || Error::NoPrevious(product.to_owned());
        let active = self.active(product)?.ok_or_else(no_previous)?;
        let previous = active.previous.ok_or_else(no_previous)?;
        self.verified(product, &previous, trusted)?;
        crate::write_json(&self.active_path(product), &Active { current: previous.clone(), previous: Some(active.current.clone()) })?;
        Ok(SwapOutcome::Swapped { from: Some(active.current), to: previous })
    }

    fn active_path(&self, product: &str) -> PathBuf {
        stage::product_dir(&self.root, product).join("active.json")
    }

    /// The staged `version`, its manifest matching and, where required, its signature trusted.
    fn verified(&self, product: &str, version: &Version, trusted: Option<&[VerifyingKey]>) -> Result<Staged> {
        let not_staged = || Error::NotStaged { product: product.to_owned(), version: version.clone() };
        let staged = stage::staged(&self.root, product, version)?.ok_or_else(not_staged)?;
        if staged.manifest.product != product || staged.manifest.version != *version {
            return Err(not_staged());
        }
        match trusted {
            Some(keys) => signature::verify_staged(&staged, keys)?,
            None if product == self.app_product => signature::verify_staged(&staged, &[])?,
            None => {}
        }
        Ok(staged)
    }

    /// Deletes version directories below both current and previous, keeping the newest `keep`.
    fn prune(&self, product: &str, active: &Active) {
        let Some(previous) = &active.previous else { return };
        let Ok(entries) = std::fs::read_dir(stage::product_dir(&self.root, product)) else { return };
        let mut older: Vec<(Version, PathBuf)> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| Some((Version::parse(e.file_name().to_str()?).ok()?, e.path())))
            .filter(|(v, _)| v < previous && *v < active.current)
            .collect();
        older.sort_by(|a, b| b.0.cmp(&a.0));
        for (version, path) in older.into_iter().skip(self.keep) {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                tracing::warn!(%product, %version, error = %e, "cannot prune old version");
            }
        }
    }
}
