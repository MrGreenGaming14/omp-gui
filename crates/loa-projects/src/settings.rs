//! Project settings and the per session exclude toggle.

use crate::{Error, Result};
use async_trait::async_trait;
use loa_core::{Dom, Draft, attr};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A file every chat of the project should know about, with why it is pinned.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Settings shared by every session of a project; `digest_model` `None` uses the summarizer's default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSettings {
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub pinned: Vec<PinnedFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_model: Option<String>,
}

/// Stores project settings; a project without any gets defaults.
#[async_trait]
pub trait SettingsStore: Send + Sync {
    async fn load(&self, project: &str) -> Result<ProjectSettings>;
    async fn save(&self, project: &str, settings: &ProjectSettings) -> Result<()>;
}

/// One JSON file per project in `dir`, written atomically, named by the project percent encoded.
#[derive(Clone, Debug)]
pub struct FileSettingsStore {
    dir: PathBuf,
}

impl FileSettingsStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, project: &str) -> PathBuf {
        let keep = |b: u8| b.is_ascii_alphanumeric() || b"-_".contains(&b);
        let name: String = project.bytes().map(|b| if keep(b) { char::from(b).to_string() } else { format!("%{b:02X}") }).collect();
        self.dir.join(name + ".json")
    }
}

#[async_trait]
impl SettingsStore for FileSettingsStore {
    async fn load(&self, project: &str) -> Result<ProjectSettings> {
        let path = self.path(project);
        match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| Error::Port(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProjectSettings::default()),
            Err(e) => Err(e.into()),
        }
    }

    async fn save(&self, project: &str, settings: &ProjectSettings) -> Result<()> {
        let path = self.path(project);
        let json = serde_json::to_vec_pretty(settings).map_err(|e| Error::Port(e.to_string()))?;
        let write = tokio::task::spawn_blocking(move || loa_util::fs::write_atomic(&path, &json, None));
        Ok(write.await.map_err(|e| Error::Port(e.to_string()))??)
    }
}

pub fn is_excluded(dom: &Dom) -> bool {
    dom.root().and_then(|r| r.bool_attr(attr::EXCLUDED)) == Some(true)
}

/// The draft that sets the exclude toggle, `None` when it already has that value.
pub fn exclude_draft(dom: &Dom, excluded: bool) -> Option<Draft> {
    let root = dom.root()?;
    (is_excluded(dom) != excluded).then(|| if excluded { Draft::set(root.id, attr::EXCLUDED, true) } else { Draft::clear(root.id, attr::EXCLUDED) })
}
