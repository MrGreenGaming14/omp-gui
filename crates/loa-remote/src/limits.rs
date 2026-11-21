//! Per host limits: allowed paths, read only, commands allowed. All checks here run before a byte
//! is sent; the symlink check that needs the remote file system lives in the service.

use serde::{Deserialize, Serialize};

use crate::audit::Action;

/// What the agent may do on one host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostLimits {
    /// Absolute path prefixes files must sit under; empty allows no file access, `/` every path.
    pub allowed_paths: Vec<String>,
    /// Refuse writes, edits and commands.
    pub read_only: bool,
    /// Whether `remote_bash` may run at all.
    pub commands: bool,
    pub auto_approve_writes: bool,
    pub auto_approve_commands: bool,
    /// Cap on captured stdout and on captured stderr, each.
    pub max_output_bytes: usize,
    pub max_file_bytes: u64,
    pub default_timeout_secs: u64,
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
            return Err(format!("host is read only; {action} refused"));
        }
        if action == Action::Command && !self.commands {
            return Err("commands are not allowed on this host".to_owned());
        }
        Ok(())
    }

    /// Refuses a normalized path outside the allowed prefixes.
    pub fn check_path(&self, normalized: &str) -> Result<(), String> {
        if self.allowed_paths.is_empty() {
            return Err(format!("{normalized} refused: no paths are allowed on this host"));
        }
        if self.prefixes().iter().any(|p| within(normalized, p)) {
            return Ok(());
        }
        Err(format!("{normalized} is outside the allowed paths ({})", self.allowed_paths.join(", ")))
    }

    /// Whether `action` needs the approval port.
    pub fn needs_approval(&self, action: Action) -> bool {
        match action {
            Action::Write | Action::Edit => !self.auto_approve_writes,
            Action::Command => !self.auto_approve_commands,
            Action::Read | Action::List | Action::Terminal => false,
        }
    }

    /// The allowed prefixes, normalized; relative ones are dropped so they never allow anything.
    pub fn prefixes(&self) -> Vec<String> {
        self.allowed_paths.iter().filter_map(|p| normalize(p).ok()).collect()
    }

    /// Whether `/` is allowed, so no path (or symlink) can escape.
    pub fn allows_every_path(&self) -> bool {
        self.prefixes().iter().any(|p| p == "/")
    }
}

/// Lexically normalizes an absolute POSIX path (`//`, `.`, `..` never above `/`); refuses relative paths and NUL.
pub fn normalize(path: &str) -> Result<String, String> {
    if path.contains('\0') {
        return Err("path contains a NUL byte".to_owned());
    }
    if !path.starts_with('/') {
        return Err(format!("path `{path}` is not absolute"));
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => drop(parts.pop()),
            p => parts.push(p),
        }
    }
    Ok(if parts.is_empty() { "/".to_owned() } else { parts.iter().map(|p| format!("/{p}")).collect() })
}

/// Whether normalized `path` is `prefix` or below it.
pub fn within(path: &str, prefix: &str) -> bool {
    prefix == "/" || path.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The directory and the last component of a normalized path.
pub(crate) fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("/", path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_actions_are_checked_before_anything_is_sent() {
        let srv = HostLimits { allowed_paths: vec!["/srv/app/".into(), "rel".into()], ..Default::default() };
        let all = HostLimits { allowed_paths: vec!["/".into()], ..Default::default() };
        for (limits, path, ok) in [
            (&srv, "/srv/app/../../etc/shadow", false),
            (&srv, "/srv/app/./a//b", true),
            (&srv, "/srv/application", false),
            (&srv, "/rel/x", false),
            (&HostLimits::default(), "/srv/x", false),
            (&all, "/etc/hosts", true),
        ] {
            assert_eq!(limits.check_path(&normalize(path).unwrap()).is_ok(), ok, "{path}");
        }
        assert!(all.allows_every_path() && !srv.allows_every_path());
        assert_eq!(normalize("/../.."), Ok("/".into()));
        assert!(normalize("relative/x").is_err() && normalize("/a\0b").is_err());
        assert_eq!((split("/a/b/c"), split("/a")), (("/a/b", "c"), ("/", "a")));
        let ro = HostLimits { read_only: true, ..Default::default() };
        let no_cmd = HostLimits { commands: false, ..Default::default() };
        let allowed = |l: &HostLimits| Action::ALL.iter().filter(|a| l.check_action(**a).is_ok()).copied().collect::<Vec<_>>();
        assert_eq!(allowed(&ro), [Action::Read, Action::List]);
        assert_eq!(allowed(&no_cmd), [Action::Read, Action::Write, Action::List, Action::Edit, Action::Terminal]);
    }
}
