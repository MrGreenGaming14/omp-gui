//! Project settings, each project's ideas notes and the per session exclude toggle.

use crate::{Error, Result};
use async_trait::async_trait;
use loa_core::{Dom, Draft, attr};
use loa_settings::projects::{Ideas, ProjectSettings};
use loa_util::fs::Snapshot;
use std::io;
use std::path::{Path, PathBuf};

/// Stores project settings and ideas notes; a project without any gets defaults and empty notes.
#[async_trait]
pub trait SettingsStore: Send + Sync {
    async fn load(&self, project: &str) -> Result<ProjectSettings>;
    async fn save(&self, project: &str, settings: &ProjectSettings) -> Result<()>;
    /// The projects that have settings, sorted by name.
    async fn names(&self) -> Result<Vec<String>>;
    /// Forgets the project's settings and ideas notes; nothing happens when it has none.
    async fn remove(&self, project: &str) -> Result<()>;
    /// The project's ideas notes as the user wrote them, empty when it has none, with the file as seen.
    async fn load_ideas(&self, project: &str) -> Result<Ideas>;
    /// Replaces the project's ideas notes with `text`, byte for byte, unless the file changed since `seen` was
    /// loaded: then [`Error::ChangedOnDisk`] and nothing is written. Returns the file as written.
    async fn save_ideas(&self, project: &str, seen: Snapshot, text: &str) -> Result<Snapshot>;
}

/// One JSON file per project in `dir`, written atomically, named by the project percent encoded. The project's
/// ideas notes are the Markdown file of the same name ending in `.ideas.md`, which the user may also edit outside.
#[derive(Clone, Debug)]
pub struct FileSettingsStore {
    dir: PathBuf,
}

impl FileSettingsStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, project: &str) -> PathBuf {
        self.file(project, ".json")
    }

    fn ideas_path(&self, project: &str) -> PathBuf {
        self.file(project, ".ideas.md")
    }

    /// The project's file in `dir`: its name percent encoded (`.` too, so no name ends like another's file), then `suffix`.
    fn file(&self, project: &str, suffix: &str) -> PathBuf {
        let keep = |b: u8| b.is_ascii_alphanumeric() || b"-_".contains(&b);
        let name: String = project.bytes().map(|b| if keep(b) { char::from(b).to_string() } else { format!("%{b:02X}") }).collect();
        self.dir.join(name + suffix)
    }
}

/// `e` naming the file it happened to.
fn at(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Port(format!("{}: {e}", path.display()))
}

/// Runs `f` off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(|e| Error::Port(e.to_string()))
}

/// Writes `bytes` to `path` with [`loa_util::fs::write_atomic`].
async fn write(path: PathBuf, bytes: Vec<u8>) -> Result<()> {
    blocking(move || loa_util::fs::write_atomic(&path, &bytes, None).map_err(|e| at(&path, e))).await?
}

/// Deletes `path`; a missing file is fine.
async fn remove_file(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(at(path, e)),
        _ => Ok(()),
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
        let json = serde_json::to_vec_pretty(settings).map_err(|e| Error::Port(e.to_string()))?;
        write(self.path(project), json).await
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

    /// The notes go first: should that fail (open elsewhere on Windows), the project is left whole.
    async fn remove(&self, project: &str) -> Result<()> {
        remove_file(&self.ideas_path(project)).await?;
        remove_file(&self.path(project)).await
    }

    async fn load_ideas(&self, project: &str) -> Result<Ideas> {
        let path = self.ideas_path(project);
        blocking(move || {
            let file = Snapshot::load(&path).map_err(|e| at(&path, e))?;
            let text = std::str::from_utf8(file.contents().unwrap_or_default()).map_err(|e| at(&path, e))?.to_owned();
            Ok(Ideas { text, file })
        }).await?
    }

    async fn save_ideas(&self, project: &str, mut seen: Snapshot, text: &str) -> Result<Snapshot> {
        let path = self.ideas_path(project);
        if seen.path() != path {
            return Err(Error::Port(format!("{} isn't the ideas notes file of {project}", seen.path().display())));
        }
        let bytes = text.as_bytes().to_vec();
        blocking(move || match seen.save(&bytes) {
            Ok(true) => Ok(seen),
            Ok(false) => Err(Error::ChangedOnDisk { path }),
            Err(e) => Err(at(&path, e)),
        }).await?
    }
}

/// Undoes the percent encoding of [`FileSettingsStore::file`]; `None` for a malformed escape or bytes that aren't UTF-8.
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
