//! Switches a product's active version between sessions, keeping the
//! previous version installed for one click rollback.
//!
//! The active version is recorded in `<root>/versions/<product>/active.json`
//! and replaced atomically, so a crash leaves either the old or the new
//! pointer, never a partial one.

use crate::fsio;
use crate::layout::{self, Staged, StagedManifest};
use crate::signature::{self, SignatureError};
use ed25519_dalek::VerifyingKey;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SwapError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path} is not valid: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{product} {version} is not fully staged")]
    NotStaged { product: String, version: Version },
    #[error("{path} describes {found_product} {found_version}, expected {product} {version}")]
    ManifestMismatch {
        path: PathBuf,
        product: String,
        version: Box<Version>,
        found_product: String,
        found_version: Box<Version>,
    },
    #[error("signature check failed: {0}")]
    Signature(#[from] SignatureError),
    #[error("{product} has no previous version to roll back to")]
    NoPrevious { product: String },
}

pub type Result<T, E = SwapError> = std::result::Result<T, E>;

/// Contents of `active.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Active {
    pub current: Version,
    pub previous: Option<Version>,
}

/// Result of a swap or rollback attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwapOutcome {
    /// A turn is running; nothing changed. Try again between sessions.
    Deferred,
    /// The requested version is already current; nothing changed.
    AlreadyActive,
    /// The active version changed from `from` (none on first install) to `to`.
    Swapped { from: Option<Version>, to: Version },
}

/// Whether the app is in the middle of a turn. Swaps only happen when no
/// turn runs.
#[async_trait::async_trait]
pub trait Activity: Send + Sync {
    fn turn_running(&self) -> bool;
    /// Resolves when no turn is running.
    async fn idle(&self);
}

/// Swaps and rolls back versions installed under `root`.
#[derive(Clone, Debug)]
pub struct Swapper {
    pub root: PathBuf,
    /// Versions older than the previous one to keep installed; older ones
    /// are deleted after a swap.
    pub keep: usize,
    /// Product id whose updates must always pass signature verification.
    pub app_product: String,
}

impl Swapper {
    /// A swapper keeping only the current and previous versions.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), keep: 0, app_product: "app".to_owned() }
    }

    /// The active pointer of `product`, if one was ever written.
    pub fn active(&self, product: &str) -> Result<Option<Active>> {
        let path = self.active_path(product);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(SwapError::Io { path, source }),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|source| SwapError::Json { path, source })
    }

    /// Payload directory of the current version of `product`.
    pub fn active_dir(&self, product: &str) -> Result<Option<PathBuf>> {
        Ok(self.active(product)?.map(|active| {
            layout::version_dir(&self.root, product, &active.current).join(layout::PAYLOAD_DIR)
        }))
    }

    /// Makes `to` the current version of `product` unless a turn is
    /// running. When `requires_signature` is given, the staged artifact
    /// must be signed by one of those keys. On any error the pointer is
    /// unchanged. After swapping, versions older than the previous one are
    /// pruned down to [`Swapper::keep`].
    pub fn try_swap(
        &self,
        product: &str,
        to: &Version,
        requires_signature: Option<&[VerifyingKey]>,
        activity: &dyn Activity,
    ) -> Result<SwapOutcome> {
        if activity.turn_running() {
            return Ok(SwapOutcome::Deferred);
        }
        let old = self.active(product)?;
        if old.as_ref().is_some_and(|a| a.current == *to) {
            return Ok(SwapOutcome::AlreadyActive);
        }
        let staged = self.staged(product, to)?;
        if product == self.app_product {
            let trusted = requires_signature.unwrap_or(&[]);
            signature::verify_staged(&staged, trusted)?;
        } else if let Some(trusted) = requires_signature {
            signature::verify_staged(&staged, trusted)?;
        }
        let from = old.map(|a| a.current);
        let active = Active { current: to.clone(), previous: from.clone() };
        self.write_active(product, &active)?;
        self.prune(product, &active);
        Ok(SwapOutcome::Swapped { from, to: to.clone() })
    }

    /// Waits until no turn is running, then swaps. If a turn starts between
    /// becoming idle and swapping, waits again.
    pub async fn swap_when_idle(
        &self,
        product: &str,
        to: &Version,
        requires_signature: Option<&[VerifyingKey]>,
        activity: &dyn Activity,
    ) -> Result<SwapOutcome> {
        loop {
            activity.idle().await;
            match self.try_swap(product, to, requires_signature, activity)? {
                SwapOutcome::Deferred => continue,
                outcome => return Ok(outcome),
            }
        }
    }

    /// Makes the previous version current again and keeps the version
    /// rolled back from as previous, so it can be re-applied. Deferred while
    /// a turn runs. App rollbacks re-verify the staged artifact.
    pub fn rollback(
        &self,
        product: &str,
        requires_signature: Option<&[VerifyingKey]>,
        activity: &dyn Activity,
    ) -> Result<SwapOutcome> {
        if activity.turn_running() {
            return Ok(SwapOutcome::Deferred);
        }
        let no_previous = || SwapError::NoPrevious { product: product.to_owned() };
        let active = self.active(product)?.ok_or_else(no_previous)?;
        let previous = active.previous.ok_or_else(no_previous)?;
        let staged = self.staged(product, &previous)?;
        if product == self.app_product {
            signature::verify_staged(&staged, requires_signature.unwrap_or(&[]))?;
        } else if let Some(trusted) = requires_signature {
            signature::verify_staged(&staged, trusted)?;
        }
        let rolled = Active { current: previous.clone(), previous: Some(active.current.clone()) };
        self.write_active(product, &rolled)?;
        Ok(SwapOutcome::Swapped { from: Some(active.current), to: previous })
    }

    fn active_path(&self, product: &str) -> PathBuf {
        layout::product_dir(&self.root, product).join(layout::ACTIVE_FILE)
    }

    fn write_active(&self, product: &str, active: &Active) -> Result<()> {
        let path = self.active_path(product);
        let bytes = serde_json::to_vec_pretty(active)
            .map_err(|source| SwapError::Json { path: path.clone(), source })?;
        fsio::write_atomic(&path, &bytes).map_err(|source| SwapError::Io { path, source })
    }

    /// Reads the manifest of a fully staged version.
    fn staged(&self, product: &str, version: &Version) -> Result<Staged> {
        let dir = layout::version_dir(&self.root, product, version);
        let path = dir.join(layout::MANIFEST_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(SwapError::NotStaged {
                    product: product.to_owned(),
                    version: version.clone(),
                });
            }
            Err(source) => return Err(SwapError::Io { path, source }),
        };
        let manifest: StagedManifest = serde_json::from_slice(&bytes)
            .map_err(|source| SwapError::Json { path: path.clone(), source })?;
        if manifest.product != product || manifest.version != *version {
            return Err(SwapError::ManifestMismatch {
                path,
                product: product.to_owned(),
                version: Box::new(version.clone()),
                found_product: manifest.product,
                found_version: Box::new(manifest.version),
            });
        }
        Ok(Staged { dir, manifest })
    }

    /// Deletes version directories older than both `active.previous` and
    /// `active.current`, keeping the newest [`Swapper::keep`] of them.
    /// Directories whose name isn't a version (temporary staging dirs) are
    /// left alone. Failures are logged: the swap itself already succeeded.
    fn prune(&self, product: &str, active: &Active) {
        let Some(previous) = &active.previous else {
            return;
        };
        let dir = layout::product_dir(&self.root, product);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "cannot list versions to prune");
                return;
            }
        };
        let mut older: Vec<(Version, PathBuf)> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if !entry.file_type().ok()?.is_dir() {
                    return None;
                }
                let version = Version::parse(entry.file_name().to_str()?).ok()?;
                (version < *previous && version < active.current).then(|| (version, entry.path()))
            })
            .collect();
        older.sort_by(|a, b| b.0.cmp(&a.0));
        for (version, path) in older.into_iter().skip(self.keep) {
            if let Err(e) = std::fs::remove_dir_all(&path) {
                tracing::warn!(%product, %version, error = %e, "cannot prune old version");
            }
        }
    }
}
