//! Versioned migration steps over the raw TOML document.

use crate::codec::version_of;
use crate::{CURRENT_VERSION, ConfigError, Result};
use toml::{Table, Value};

/// Rewrites a document of version `from` into version `from + 1`. The
/// `version` key is updated by the [`Migrator`], not by the step.
pub type StepFn = fn(&mut Table) -> Result<(), String>;

/// One migration from version `from` to `from + 1`.
#[derive(Clone, Copy, Debug)]
pub struct Step {
    pub from: u32,
    /// One line on what the step changes, for logs and the backup notice.
    pub summary: &'static str,
    pub apply: StepFn,
}

/// Migrations shipped with the app. Schema v1 is the first version, so the
/// chain is empty; a v2 schema adds `Step { from: 1, .. }` here.
const STANDARD_STEPS: &[Step] = &[];

/// A chain of [`Step`]s bringing a document up to a target version.
#[derive(Clone, Debug)]
pub struct Migrator {
    target: u32,
    steps: Vec<Step>,
}

impl Migrator {
    /// The app's own chain, ending at [`CURRENT_VERSION`].
    pub fn standard() -> Self {
        Self { target: CURRENT_VERSION, steps: STANDARD_STEPS.to_vec() }
    }

    /// An empty chain ending at `target`.
    pub fn new(target: u32) -> Self {
        Self { target, steps: Vec::new() }
    }

    /// Adds a step. A later step with the same `from` replaces an earlier one.
    #[must_use]
    pub fn step(mut self, step: Step) -> Self {
        self.steps.retain(|s| s.from != step.from);
        self.steps.push(step);
        self
    }

    /// The version every migrated document ends at.
    pub fn target(&self) -> u32 {
        self.target
    }

    /// Brings `doc` up to [`Self::target`]. Returns the version it started at
    /// when any step ran. A document newer than the target is refused rather
    /// than downgraded. On error `doc` may be partly migrated and must be
    /// discarded.
    pub fn migrate(&self, doc: &mut Table) -> Result<Option<u32>> {
        let start = version_of(doc)?;
        if start > self.target {
            return Err(ConfigError::TooNew { found: start, supported: self.target });
        }
        for from in start..self.target {
            let step = self
                .steps
                .iter()
                .find(|s| s.from == from)
                .ok_or(ConfigError::MissingMigration { from })?;
            (step.apply)(doc).map_err(|reason| ConfigError::Migration { from, reason })?;
            doc.insert("version".to_owned(), Value::Integer(i64::from(from + 1)));
        }
        Ok((start < self.target).then_some(start))
    }
}
