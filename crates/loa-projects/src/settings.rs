//! Project settings and the per session exclude toggle.

use crate::{Error, Result};
use async_trait::async_trait;
use loa_core::{Dom, Draft, attr};
use loa_util::LockExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A file every chat of the project should know about.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedFile {
    pub path: String,
    /// Why it is pinned, shown next to the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Settings shared by every session of a project, like a CLAUDE.md for the whole project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSettings {
    /// Markdown instructions delivered with the project context.
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub pinned: Vec<PinnedFile>,
    /// Model or role for digests; `None` uses the summarizer's default (`@smol`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_model: Option<String>,
}

/// Stores project settings; a project without any gets defaults.
#[async_trait]
pub trait SettingsStore: Send + Sync {
    async fn load(&self, project: &str) -> Result<ProjectSettings>;
    async fn save(&self, project: &str, settings: &ProjectSettings) -> Result<()>;
}

/// Settings kept in memory, for previews and tests.
#[derive(Debug, Default)]
pub struct MemorySettingsStore(Mutex<HashMap<String, ProjectSettings>>);

impl MemorySettingsStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SettingsStore for MemorySettingsStore {
    async fn load(&self, project: &str) -> Result<ProjectSettings> {
        Ok(self.0.locked().get(project).cloned().unwrap_or_default())
    }

    async fn save(&self, project: &str, settings: &ProjectSettings) -> Result<()> {
        self.0.locked().insert(project.to_owned(), settings.clone());
        Ok(())
    }
}

/// One JSON file per project in a directory, written atomically, named by the
/// project id with every byte but `[A-Za-z0-9_-]` percent encoded.
#[derive(Clone, Debug)]
pub struct FileSettingsStore {
    dir: PathBuf,
}

impl FileSettingsStore {
    /// `dir` is created on the first save.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
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
