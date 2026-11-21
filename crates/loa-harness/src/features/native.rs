//! Native harness sessions on this machine, for the import picker: each
//! harness's [`NativeCodec::discover`], plus recognizing a picked file.

#[allow(unused_imports)]
use crate::kit::NativeCodec;
pub use crate::kit::NativeSession;
use std::io::Read;
use std::path::Path;

/// Harness ids, as [`loa_contracts::harness::Adapter::harness`] returns them.
pub const OMP: &str = "omp";
pub const CLAUDE: &str = "claude-code";
pub const CODEX: &str = "codex";
pub const OPENCODE: &str = "opencode";

/// Every native session under `home`, newest first. OpenCode sessions are picked by path.
pub fn discover(home: &Path) -> Vec<NativeSession> {
    let mut out: Vec<NativeSession> = Vec::new();
    #[cfg(feature = "omp")]
    out.extend(crate::omp::OmpNative::discover(home));
    #[cfg(feature = "claude")]
    out.extend(crate::claude::native::ClaudeNative::discover(home));
    #[cfg(feature = "codex")]
    out.extend(crate::codex::rollout::CodexNative::discover(home));
    let _ = home;
    out.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.path.cmp(&b.path)));
    out
}

/// The harness a session file belongs to, from where it is and how it starts.
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
    let mut head = Vec::with_capacity(4096);
    std::fs::File::open(path).ok()?.take(4096).read_to_end(&mut head).ok()?;
    let head = String::from_utf8_lossy(&head);
    if path.extension().is_some_and(|e| e == "json") && head.trim_start().starts_with('{') && head.contains("\"info\"") {
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
