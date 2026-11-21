//! Project settings and the per session exclude toggle.

use crate::{Error, Result};
use async_trait::async_trait;
use loa_core::{Dom, Draft, attr};
use loa_settings::projects::ProjectSettings;
use std::path::PathBuf;

/// Stores project settings; a project without any gets defaults.
#[async_trait]
pub trait SettingsStore: Send + Sync {
    async fn load(&self, project: &str) -> Result<ProjectSettings>;
    async fn save(&self, project: &str, settings: &ProjectSettings) -> Result<()>;
    /// The projects that have settings, sorted by name.
    async fn names(&self) -> Result<Vec<String>>;
    /// Forgets the project's settings; nothing happens when it has none.
    async fn remove(&self, project: &str) -> Result<()>;
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

    async fn names(&self) -> Result<Vec<String>> {
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let file = entry.file_name();
            let Some(name) = file.to_str().and_then(|f| f.strip_suffix(".json")).and_then(decode_name) else { continue };
            // Only the file `path` writes for a name counts, so strays are skipped.
            if self.path(&name).file_name() == Some(file.as_os_str()) && entry.file_type().await?.is_file() {
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

/// Undoes the percent encoding of [`FileSettingsStore::path`]; `None` for a malformed escape or bytes that aren't UTF-8.
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

pub fn is_excluded(dom: &Dom) -> bool {
    dom.root().and_then(|r| r.bool_attr(attr::EXCLUDED)) == Some(true)
}

/// The draft that sets the exclude toggle, `None` when it already has that value.
pub fn exclude_draft(dom: &Dom, excluded: bool) -> Option<Draft> {
    let root = dom.root()?;
    (is_excluded(dom) != excluded).then(|| if excluded { Draft::set(root.id, attr::EXCLUDED, true) } else { Draft::clear(root.id, attr::EXCLUDED) })
}
