//! Project settings and the per session exclude toggle.

use crate::Result;
use async_trait::async_trait;
use loa_core::{Dom, Draft, attr};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// A file every chat of the project should know about.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedFile {
    pub path: String,
    /// Why it is pinned, shown next to the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Settings shared by every session of a project, like a CLAUDE.md for the
/// whole project.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSettings {
    /// Markdown instructions delivered with the project context.
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub pinned: Vec<PinnedFile>,
    /// Model or role for digests (`--model` of the omp digester). `None`
    /// uses the `smol` role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digest_model: Option<String>,
}

/// Stores project settings. `loa-app` backs it with its config or SQLite.
#[async_trait]
pub trait SettingsStore: Send + Sync {
    /// The project's settings; defaults when it has none.
    async fn load(&self, project: &str) -> Result<ProjectSettings>;
    async fn save(&self, project: &str, settings: &ProjectSettings) -> Result<()>;
}

/// Settings kept in memory, for previews and tests.
#[derive(Debug, Default)]
pub struct MemorySettingsStore {
    projects: Mutex<HashMap<String, ProjectSettings>>,
}

impl MemorySettingsStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, ProjectSettings>> {
        // Inserting or cloning a value can't leave the map half written.
        self.projects.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[async_trait]
impl SettingsStore for MemorySettingsStore {
    async fn load(&self, project: &str) -> Result<ProjectSettings> {
        Ok(self.lock().get(project).cloned().unwrap_or_default())
    }

    async fn save(&self, project: &str, settings: &ProjectSettings) -> Result<()> {
        self.lock().insert(project.to_owned(), settings.clone());
        Ok(())
    }
}

/// Whether the session is excluded from project context.
pub fn is_excluded(dom: &Dom) -> bool {
    dom.root().and_then(|r| r.bool_attr(attr::EXCLUDED)) == Some(true)
}

/// The draft that sets the exclude toggle, or `None` when it already has
/// that value (or there is no `<session>`). Including clears the attribute.
pub fn exclude_draft(dom: &Dom, excluded: bool) -> Option<Draft> {
    let root = dom.root()?;
    if is_excluded(dom) == excluded {
        return None;
    }
    Some(if excluded { Draft::set(root.id, attr::EXCLUDED, true) } else { Draft::clear(root.id, attr::EXCLUDED) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::testing::Builder;

    #[test]
    fn exclude_toggle_round_trips() {
        let b = Builder::new("s1", "p", 1);
        assert!(exclude_draft(&b.dom(), false).is_none());
        b.commit(exclude_draft(&b.dom(), true).unwrap());
        assert!(is_excluded(&b.dom()));
        assert!(exclude_draft(&b.dom(), true).is_none());
        b.commit(exclude_draft(&b.dom(), false).unwrap());
        assert!(!is_excluded(&b.dom()));
    }
}
