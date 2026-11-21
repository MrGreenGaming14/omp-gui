//! Native harness sessions on this machine, for the import picker.
//!
//! Where each harness keeps them:
//! - omp: `~/.omp/agent/sessions/<project>/<name>.jsonl`
//! - Claude Code: `~/.claude/projects/<project>/<uuid>.jsonl`
//! - Codex: `~/.codex/sessions/<yyyy>/<mm>/<dd>/rollout-*.jsonl`
//! - OpenCode keeps sessions in its own storage; it imports the file
//!   `opencode export <id>` writes, so those are picked by path.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Harness ids, as [`loa_contracts::harness::Adapter::harness`] returns them.
pub const OMP: &str = "omp";
pub const CLAUDE: &str = "claude-code";
pub const CODEX: &str = "codex";
pub const OPENCODE: &str = "opencode";

/// One native session file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeSession {
    pub harness: &'static str,
    pub path: PathBuf,
    /// The harness's project folder name, when it groups sessions by one.
    pub project: Option<String>,
    /// omp's session title, when the file has one.
    pub title: Option<String>,
    /// Unix milliseconds of the last change.
    pub modified: i64,
    pub size: u64,
}

/// Every native session under `home`, newest first.
pub fn discover(home: &Path) -> Vec<NativeSession> {
    let mut out = Vec::new();
    for (harness, root) in [(OMP, home.join(".omp/agent/sessions")), (CLAUDE, home.join(".claude/projects"))] {
        for project in dirs(&root) {
            let name = project.file_name().map(|n| n.to_string_lossy().into_owned());
            for file in files(&project, |f| f.ends_with(".jsonl")) {
                out.extend(entry(harness, file, name.clone()));
            }
        }
    }
    let mut stack = vec![home.join(".codex/sessions")];
    while let Some(dir) = stack.pop() {
        stack.extend(dirs(&dir));
        for file in files(&dir, |f| f.starts_with("rollout-") && f.ends_with(".jsonl")) {
            out.extend(entry(CODEX, file, None));
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.path.cmp(&b.path)));
    out
}

fn dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect()
}

fn files(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && keep(&e.file_name().to_string_lossy()))
        .map(|e| e.path())
        .collect()
}

fn entry(harness: &'static str, path: PathBuf, project: Option<String>) -> Option<NativeSession> {
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let title = (harness == OMP).then(|| omp_title(&path)).flatten();
    Some(NativeSession { harness, path, project, title, modified, size: meta.len() })
}

/// The first bytes of a file, enough for its first line or header.
fn head(path: &Path) -> Option<String> {
    let mut buf = Vec::with_capacity(4096);
    std::fs::File::open(path).ok()?.take(4096).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// omp writes a `{"type":"title",…}` line first.
fn omp_title(path: &Path) -> Option<String> {
    let head = head(path)?;
    let line: serde_json::Value = serde_json::from_str(head.lines().next()?).ok()?;
    (line.get("type")?.as_str()? == "title").then(|| line.get("title")?.as_str().map(str::to_owned)).flatten()
}

/// The harness a session file at `path` belongs to, from where it is and how
/// it starts. `None` when it isn't recognized.
pub fn harness_of(path: &Path) -> Option<&'static str> {
    let text = path.to_string_lossy();
    if text.contains("/.omp/") {
        return Some(OMP);
    }
    if text.contains("/.claude/") {
        return Some(CLAUDE);
    }
    if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("rollout-")) {
        return Some(CODEX);
    }
    let head = head(path)?;
    let trimmed = head.trim_start();
    if path.extension().is_some_and(|e| e == "json") && trimmed.starts_with('{') && head.contains("\"info\"") {
        return Some(OPENCODE);
    }
    let first: serde_json::Value = serde_json::from_str(head.lines().next()?).ok()?;
    match first.get("type").and_then(serde_json::Value::as_str) {
        Some("title" | "session") => Some(OMP),
        Some("session_meta") => Some(CODEX),
        _ if first.get("sessionId").is_some() => Some(CLAUDE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn finds_each_harness_in_its_place_and_recognizes_picked_files() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        let omp = h.join(".omp/agent/sessions/-Projects-a/2026_x.jsonl");
        write(&omp, "{\"type\":\"title\",\"v\":1,\"title\":\"Sorter\"}\n{\"type\":\"session\"}\n");
        let claude = h.join(".claude/projects/-home-a/u.jsonl");
        write(&claude, "{\"type\":\"user\",\"sessionId\":\"u\"}\n");
        let codex = h.join(".codex/sessions/2026/11/19/rollout-1.jsonl");
        write(&codex, "{\"type\":\"session_meta\"}\n");
        write(&h.join(".codex/sessions/2026/11/19/other.txt"), "x");
        let found = discover(h);
        assert_eq!(found.len(), 3, "{found:?}");
        let omp_entry = found.iter().find(|s| s.harness == OMP).unwrap();
        assert_eq!(omp_entry.title.as_deref(), Some("Sorter"));
        assert_eq!(omp_entry.project.as_deref(), Some("-Projects-a"));
        assert!(found.iter().any(|s| s.harness == CLAUDE && s.path == claude));
        assert!(found.iter().any(|s| s.harness == CODEX && s.path == codex));

        let picked = h.join("elsewhere/export.json");
        write(&picked, "{\"info\":{\"id\":\"ses_1\"},\"messages\":[]}");
        assert_eq!(harness_of(&picked), Some(OPENCODE));
        let moved = h.join("elsewhere/omp.jsonl");
        write(&moved, "{\"type\":\"session\",\"version\":3}\n");
        assert_eq!(harness_of(&moved), Some(OMP));
        let rollout = h.join("elsewhere/rollout-9.jsonl");
        write(&rollout, "{}\n");
        assert_eq!(harness_of(&rollout), Some(CODEX));
        let unknown = h.join("elsewhere/notes.jsonl");
        write(&unknown, "{\"a\":1}\n");
        assert_eq!(harness_of(&unknown), None);
    }
}
