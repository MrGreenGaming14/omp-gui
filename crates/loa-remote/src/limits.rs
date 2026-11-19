//! Per host limits (#100): allowed paths, read only, commands allowed.
//!
//! Everything here runs before a byte is sent to the host. The symlink check
//! that needs the remote file system lives in the service and runs before
//! any read or write.

use serde::{Deserialize, Serialize};

use crate::audit::Action;

/// What the agent may do on one host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostLimits {
    /// Absolute path prefixes files must sit under. Empty allows any path.
    pub allowed_paths: Vec<String>,
    /// Refuse writes, edits and commands.
    pub read_only: bool,
    /// Whether `remote_bash` may run at all.
    pub commands: bool,
    /// Writes and edits run without an approval card.
    pub auto_approve_writes: bool,
    /// Commands run without an approval card.
    pub auto_approve_commands: bool,
    /// Cap on captured stdout and on captured stderr, each.
    pub max_output_bytes: usize,
    /// Cap on a file read or written in one call.
    pub max_file_bytes: u64,
    /// Command timeout when the call gives none.
    pub default_timeout_secs: u64,
    /// Upper bound on any timeout the call asks for.
    pub max_timeout_secs: u64,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self {
            allowed_paths: Vec::new(),
            read_only: false,
            commands: true,
            auto_approve_writes: false,
            auto_approve_commands: false,
            max_output_bytes: 256 * 1024,
            max_file_bytes: 8 * 1024 * 1024,
            default_timeout_secs: 120,
            max_timeout_secs: 3600,
        }
    }
}

impl HostLimits {
    /// Refuses actions the host does not allow at all.
    pub fn check_action(&self, action: Action) -> Result<(), String> {
        if self.read_only && action.mutates() {
            return Err(format!("host is read only; {} refused", action.as_str()));
        }
        if action == Action::Command && !self.commands {
            return Err("commands are not allowed on this host".to_string());
        }
        Ok(())
    }

    /// Refuses a normalized path outside the allowed prefixes.
    pub fn check_path(&self, normalized: &str) -> Result<(), String> {
        if self.allowed_paths.is_empty() || self.allowed_by_any(normalized, &self.prefixes()) {
            Ok(())
        } else {
            Err(format!(
                "{normalized} is outside the allowed paths ({})",
                self.allowed_paths.join(", ")
            ))
        }
    }

    /// Whether `action` needs the approval port.
    pub fn needs_approval(&self, action: Action) -> bool {
        match action {
            Action::Write | Action::Edit => !self.auto_approve_writes,
            Action::Command => !self.auto_approve_commands,
            Action::Read | Action::List => false,
        }
    }

    /// The allowed prefixes, normalized. Prefixes that do not normalize
    /// (relative ones) are dropped, so they never allow anything.
    pub fn prefixes(&self) -> Vec<String> {
        self.allowed_paths
            .iter()
            .filter_map(|p| normalize(p).ok())
            .collect()
    }

    pub(crate) fn allowed_by_any(&self, path: &str, prefixes: &[String]) -> bool {
        prefixes.iter().any(|p| within(path, p))
    }
}

/// Lexically normalizes an absolute POSIX path: collapses `//` and `.`,
/// applies `..` (never above `/`). Relative paths and NUL bytes are refused.
pub fn normalize(path: &str) -> Result<String, String> {
    if path.contains('\0') {
        return Err("path contains a NUL byte".to_string());
    }
    if !path.starts_with('/') {
        return Err(format!("path `{path}` is not absolute"));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = String::with_capacity(path.len());
    for p in &parts {
        out.push('/');
        out.push_str(p);
    }
    if out.is_empty() {
        out.push('/');
    }
    Ok(out)
}

/// Whether normalized `path` is `prefix` or below it.
pub fn within(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// The directory part of a normalized path.
pub(crate) fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

/// The last component of a normalized path.
pub(crate) fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotdot_cannot_escape_an_allowed_prefix() {
        let limits = HostLimits {
            allowed_paths: vec!["/srv/app/".into()],
            ..Default::default()
        };
        let escape = normalize("/srv/app/../../etc/shadow").unwrap();
        assert_eq!(escape, "/etc/shadow");
        assert!(limits.check_path(&escape).is_err());
        assert!(limits.check_path(&normalize("/srv/app/./a//b").unwrap()).is_ok());
        assert!(limits.check_path("/srv/app").is_ok());
        assert!(
            limits.check_path("/srv/application").is_err(),
            "prefix must end on a component boundary"
        );
        assert_eq!(normalize("/../..").unwrap(), "/");
        assert!(normalize("relative/x").is_err());
        assert!(normalize("/a\0b").is_err());
    }

    #[test]
    fn read_only_refuses_mutations_and_commands_flag_is_separate() {
        let ro = HostLimits {
            read_only: true,
            ..Default::default()
        };
        assert!(ro.check_action(Action::Read).is_ok());
        assert!(ro.check_action(Action::List).is_ok());
        assert!(ro.check_action(Action::Write).is_err());
        assert!(ro.check_action(Action::Edit).is_err());
        assert!(ro.check_action(Action::Command).is_err());
        let no_cmd = HostLimits {
            commands: false,
            ..Default::default()
        };
        assert!(no_cmd.check_action(Action::Write).is_ok());
        assert!(no_cmd.check_action(Action::Command).is_err());
    }

    #[test]
    fn relative_allowed_prefix_allows_nothing() {
        let limits = HostLimits {
            allowed_paths: vec!["srv".into()],
            ..Default::default()
        };
        assert!(limits.check_path("/srv/x").is_err());
    }

    #[test]
    fn parent_and_name() {
        assert_eq!(parent("/a/b/c"), "/a/b");
        assert_eq!(parent("/a"), "/");
        assert_eq!(file_name("/a/b/c"), "c");
    }
}
