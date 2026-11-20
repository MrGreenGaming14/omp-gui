//! Project settings and the per session exclude toggle.

use crate::{Error, Result};
use async_trait::async_trait;
use loa_core::{Dom, Draft, attr};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Write as _;
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
    /// Working directory of the project, set by `ProjectService::create`.
    /// `None` when the project has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<PathBuf>,
}

/// Stores project settings. `loa-app` backs it with its config or SQLite.
#[async_trait]
pub trait SettingsStore: Send + Sync {
    /// The project's settings; defaults when it has none.
    async fn load(&self, project: &str) -> Result<ProjectSettings>;
    async fn save(&self, project: &str, settings: &ProjectSettings) -> Result<()>;
    /// The projects that have settings, sorted by name.
    async fn names(&self) -> Result<Vec<String>>;
    /// Forgets the project's settings; nothing happens when it has none.
    async fn remove(&self, project: &str) -> Result<()>;
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

    async fn names(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = self.lock().keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    async fn remove(&self, project: &str) -> Result<()> {
        self.lock().remove(project);
        Ok(())
    }
}

/// Settings kept as one JSON file per project in a directory, written
/// atomically. The file name is the project id, percent encoded.
#[derive(Clone, Debug)]
pub struct FileSettingsStore {
    dir: PathBuf,
}

impl FileSettingsStore {
    /// A store in `dir`, created on the first save.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, project: &str) -> PathBuf {
        let mut name = String::with_capacity(project.len() + 5);
        for byte in project.bytes() {
            if byte.is_ascii_alphanumeric() || b"-_".contains(&byte) {
                name.push(byte as char);
            } else {
                let _ = write!(name, "%{byte:02X}");
            }
        }
        name.push_str(".json");
        self.dir.join(name)
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
        let dir = self.dir.clone();
        let json = serde_json::to_vec_pretty(settings).map_err(|e| Error::Port(e.to_string()))?;
        tokio::task::spawn_blocking(move || -> Result<()> {
            std::fs::create_dir_all(&dir)?;
            let mut file = tempfile::NamedTempFile::new_in(&dir)?;
            std::io::Write::write_all(&mut file, &json)?;
            file.as_file().sync_all()?;
            file.persist(&path).map_err(|e| e.error)?;
            Ok(())
        })
        .await
        .map_err(|e| Error::Port(e.to_string()))?
    }

    async fn names(&self) -> Result<Vec<String>> {
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let file = entry.file_name();
            let Some(name) = file.to_str().and_then(|f| f.strip_suffix(".json")).and_then(decode_name) else {
                continue;
            };
            // Only the file `path` writes for a name counts, so strays are skipped.
            if self.path(&name).file_name() != Some(file.as_os_str()) {
                continue;
            }
            if entry.file_type().await?.is_file() {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    async fn remove(&self, project: &str) -> Result<()> {
        match tokio::fs::remove_file(self.path(project)).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// Undoes the encoding `FileSettingsStore::path` applies to a file name stem,
/// or `None` for a malformed escape or bytes that are not UTF-8.
fn decode_name(stem: &str) -> Option<String> {
    let mut out = Vec::with_capacity(stem.len());
    let mut bytes = stem.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hex = [bytes.next()?, bytes.next()?];
            out.push(u8::from_str_radix(std::str::from_utf8(&hex).ok()?, 16).ok()?);
        } else {
            out.push(byte);
        }
    }
    String::from_utf8(out).ok()
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

    #[tokio::test]
    async fn file_store_round_trips_and_defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSettingsStore::new(dir.path().join("projects"));
        assert_eq!(store.load("my/proj").await.unwrap(), ProjectSettings::default());
        let settings = ProjectSettings {
            instructions: "Use rustls.".into(),
            pinned: vec![PinnedFile { path: "docs/a.md".into(), note: Some("contract".into()) }],
            digest_model: Some("@smol".into()),
            folder: Some("/work/my-proj".into()),
        };
        store.save("my/proj", &settings).await.unwrap();
        assert_eq!(store.load("my/proj").await.unwrap(), settings);
        assert!(dir.path().join("projects/my%2Fproj.json").is_file());
        assert_eq!(FileSettingsStore::new(dir.path().join("projects")).load("my/proj").await.unwrap(), settings);
        assert_eq!(store.load("other").await.unwrap(), ProjectSettings::default());
    }

    #[tokio::test]
    async fn file_store_names_lists_projects_and_skips_strays() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects");
        let store = FileSettingsStore::new(&projects);
        assert!(store.names().await.unwrap().is_empty());
        let saved = ["my/proj", "with space", "a.b", ".", "héllo", "loa"];
        for name in saved {
            store.save(name, &ProjectSettings::default()).await.unwrap();
        }
        let mut expected = saved.to_vec();
        expected.sort();
        assert_eq!(store.names().await.unwrap(), expected);
        // Strays: no `.json` suffix, a bad escape, a non-canonical escape, a directory.
        std::fs::write(projects.join("notes.txt"), "x").unwrap();
        std::fs::write(projects.join("bad%zz.json"), "{}").unwrap();
        std::fs::write(projects.join("my%2fproj.json"), "{}").unwrap();
        std::fs::create_dir(projects.join("dir.json")).unwrap();
        assert_eq!(store.names().await.unwrap(), expected);
        let memory = MemorySettingsStore::new();
        memory.save("b", &ProjectSettings::default()).await.unwrap();
        memory.save("a", &ProjectSettings::default()).await.unwrap();
        assert_eq!(memory.names().await.unwrap(), ["a", "b"]);
    }
}
