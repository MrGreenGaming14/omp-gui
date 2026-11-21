//! Where omp keeps its `config.yml` and `models.yml`, and the port that asks
//! omp for its active agent directory.

use std::path::PathBuf;

/// omp's settings files in its active agent directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmpFiles {
    pub agent_dir: PathBuf,
}

impl OmpFiles {
    pub fn new(agent_dir: impl Into<PathBuf>) -> Self {
        Self { agent_dir: agent_dir.into() }
    }

    /// `config.yml`: roles, fallback chains, enabled models and providers.
    pub fn config(&self) -> PathBuf {
        self.agent_dir.join("config.yml")
    }

    /// `models.yml`: custom providers and model overrides.
    pub fn models(&self) -> PathBuf {
        self.agent_dir.join("models.yml")
    }
}

/// Finds omp's active agent directory the way omp itself does (profiles,
/// `PI_CODING_AGENT_DIR`).
#[async_trait::async_trait]
pub trait OmpFilesResolver: Send + Sync {
    async fn resolve(&self) -> crate::error::Result<OmpFiles>;
}
