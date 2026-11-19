//! Session format version and config schema checks.

use super::{CheckResult, GateCheck};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Config keys a version accepts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSchema {
    /// Dotted key paths, e.g. `memory.backend`.
    pub keys: BTreeSet<String>,
    /// Keys whose children are free form (records such as `modelRoles`):
    /// any key below one of them is accepted, e.g. `modelRoles.smol`.
    #[serde(default)]
    pub open: BTreeSet<String>,
    /// Aliases the version declares: old key -> new key.
    pub renamed: BTreeMap<String, String>,
}

impl ConfigSchema {
    /// Whether `key` is a known key or lies below an open one.
    pub fn accepts(&self, key: &str) -> bool {
        self.keys.contains(key)
            || key.match_indices('.').any(|(at, _)| self.open.contains(&key[..at]))
    }
}

/// Asks an installed version for its session format and config schema.
#[async_trait::async_trait]
pub trait FormatProbe: Send + Sync {
    async fn session_version(&self, executable: &Path) -> Result<u32, String>;
    async fn config_keys(&self, executable: &Path) -> Result<ConfigSchema, String>;
}

/// Harmful when the session format version changes and the adapter's
/// migrations (`(from, to)` steps) offer no chain from `old` to `new`.
pub fn check_session_format(old: u32, new: u32, migrations: &BTreeSet<(u32, u32)>) -> CheckResult {
    if old == new {
        return CheckResult::pass(GateCheck::SessionFormat, vec![format!("unchanged at {old}")]);
    }
    match migration_path(old, new, migrations) {
        Some(path) => {
            let steps: Vec<String> = path.iter().map(u32::to_string).collect();
            CheckResult::pass(
                GateCheck::SessionFormat,
                vec![format!("~ {old} -> {new}: migrated via {}", steps.join(" -> "))],
            )
        }
        None => CheckResult::harmful(
            GateCheck::SessionFormat,
            vec![format!("~ {old} -> {new}: no migration")],
        ),
    }
}

/// Shortest chain of migration steps from `from` to `to`, as the list of
/// versions visited (both ends included).
fn migration_path(from: u32, to: u32, migrations: &BTreeSet<(u32, u32)>) -> Option<Vec<u32>> {
    let mut previous: BTreeMap<u32, u32> = BTreeMap::new();
    let mut queue = std::collections::VecDeque::from([from]);
    while let Some(at) = queue.pop_front() {
        if at == to {
            let mut path = vec![to];
            let mut cur = to;
            while let Some(&prev) = previous.get(&cur) {
                path.push(prev);
                cur = prev;
            }
            path.reverse();
            return Some(path);
        }
        for &(a, b) in migrations.range((at, u32::MIN)..=(at, u32::MAX)) {
            debug_assert_eq!(a, at);
            if b != from && !previous.contains_key(&b) {
                previous.insert(b, at);
                queue.push_back(b);
            }
        }
    }
    None
}

/// Harmful when a key the app writes is absent from the new schema.
pub fn check_config_schema(written_keys: &BTreeSet<String>, new: &ConfigSchema) -> CheckResult {
    let mut details = Vec::new();
    for key in written_keys {
        if new.accepts(key) {
            continue;
        }
        match new.renamed.get(key) {
            Some(to) => details.push(format!("- {key}: renamed to {to}")),
            None => details.push(format!("- {key}: removed")),
        }
    }
    if details.is_empty() {
        CheckResult::pass(GateCheck::ConfigSchema, details)
    } else {
        CheckResult::harmful(GateCheck::ConfigSchema, details)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gates::Verdict;

    fn set(steps: &[(u32, u32)]) -> BTreeSet<(u32, u32)> {
        steps.iter().copied().collect()
    }

    #[test]
    fn session_migrations_chain() {
        let m = set(&[(2, 3), (3, 4)]);
        assert_eq!(check_session_format(3, 3, &BTreeSet::new()).verdict, Verdict::Pass);
        assert_eq!(check_session_format(2, 4, &m).verdict, Verdict::Pass);
        assert_eq!(check_session_format(3, 4, &m).verdict, Verdict::Pass);
        assert_eq!(check_session_format(3, 5, &m).verdict, Verdict::Harmful);
        assert_eq!(check_session_format(4, 2, &m).verdict, Verdict::Harmful);
        let cyclic = set(&[(3, 2), (2, 3)]);
        assert_eq!(check_session_format(3, 4, &cyclic).verdict, Verdict::Harmful);
    }

    #[test]
    fn config_rename_vs_removal() {
        let written: BTreeSet<String> =
            ["a.b", "keep", "gone"].into_iter().map(String::from).collect();
        let schema = ConfigSchema {
            keys: ["a.c", "keep", "extra"].into_iter().map(String::from).collect(),
            open: BTreeSet::new(),
            renamed: [("a.b".to_string(), "a.c".to_string())].into(),
        };
        let r = check_config_schema(&written, &schema);
        assert_eq!(r.verdict, Verdict::Harmful);
        assert_eq!(r.details, vec!["- a.b: renamed to a.c", "- gone: removed"]);

        let ok: BTreeSet<String> = ["keep".to_string()].into();
        assert_eq!(check_config_schema(&ok, &schema).verdict, Verdict::Pass);
    }

    #[test]
    fn keys_below_open_records_are_accepted() {
        let schema = ConfigSchema {
            keys: ["modelRoles", "memory.backend"].into_iter().map(String::from).collect(),
            open: ["modelRoles".to_string()].into(),
            renamed: BTreeMap::new(),
        };
        let written: BTreeSet<String> =
            ["modelRoles.smol", "modelRoles", "memory.backend"].into_iter().map(String::from).collect();
        assert_eq!(check_config_schema(&written, &schema).verdict, Verdict::Pass);
        let below_closed: BTreeSet<String> = ["memory.backend.x".to_string()].into();
        assert_eq!(check_config_schema(&below_closed, &schema).verdict, Verdict::Harmful);
        let lookalike: BTreeSet<String> = ["modelRolesX.smol".to_string()].into();
        assert_eq!(check_config_schema(&lookalike, &schema).verdict, Verdict::Harmful);
    }
}
