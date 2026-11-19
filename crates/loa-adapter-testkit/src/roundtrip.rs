//! Import a native session, export it, diff the export against the original,
//! re-import the export and compare the two DOMs.

use crate::caps::check_produced;
use crate::check::{check_dom, check_history};
use crate::diff::{FileDiff, diff_paths};
use crate::golden::{diff_renderings, render_golden};
use crate::violation::{Rule, Violation, write_list};
use crate::{Error, Result};
use loa_adapter::Adapter;
use loa_core::{Dom, MemorySink, Patch};
use std::fmt;
use std::path::{Path, PathBuf};

/// Everything a round trip found. Clean when `violations` is empty.
#[derive(Debug)]
pub struct RoundTrip {
    pub native: PathBuf,
    /// The DOM of the first import, for golden checks.
    pub imported: Dom,
    /// The patches of the first import.
    pub patches: Vec<Patch>,
    /// Convention checks on the import, one `Export` violation per differing
    /// file, and a `Reimport` violation when the DOMs differ.
    pub violations: Vec<Violation>,
    /// Differences between the native file and the export, per file.
    pub export_diffs: Vec<FileDiff>,
    /// Unified diff of the golden renderings of the import and the re-import.
    pub reimport_diff: String,
}

impl RoundTrip {
    pub fn is_clean(&self) -> bool {
        self.violations.is_empty()
    }
}

impl fmt::Display for RoundTrip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "round trip of {}:", self.native.display())?;
        write_list(f, &self.violations)?;
        for diff in &self.export_diffs {
            write!(f, "  {diff}")?;
        }
        if !self.reimport_diff.is_empty() {
            writeln!(f, "  re-import differs:")?;
            for line in self.reimport_diff.lines() {
                writeln!(f, "    {line}")?;
            }
        }
        Ok(())
    }
}

/// Runs the round trip of `native` (a file or a directory) through `adapter`.
/// `Err` when the adapter fails to import or export or a file can't be read;
/// a broken round trip is reported in the returned [`RoundTrip`].
pub fn round_trip(adapter: &dyn Adapter, native: &Path) -> Result<RoundTrip> {
    let first = MemorySink::new();
    adapter.import(native, &first)?;
    let imported = first.dom();
    let patches = first.patches();

    let mut violations = check_dom(&imported);
    violations.extend(check_history(&patches));
    violations.extend(check_produced(&imported, adapter.harness(), &adapter.capabilities()));

    let scratch = tempfile::tempdir().map_err(|source| Error::io(Path::new("temp dir"), source))?;
    let name = native.file_name().map_or_else(|| "export".into(), ToOwned::to_owned);
    let out = scratch.path().join(name);
    adapter.export(&imported, &out)?;

    let export_diffs = diff_paths(native, &out)?;
    for diff in &export_diffs {
        let name = if diff.path.as_os_str().is_empty() { native.display().to_string() } else { diff.path.display().to_string() };
        violations.push(Violation::new(
            Rule::Export,
            None,
            format!("{name}: {} difference(s) after export", diff.differences.len() + diff.more),
        ));
    }

    let second = MemorySink::new();
    adapter.import(&out, &second)?;
    let reimported = second.dom();
    let reimport_diff = if imported == reimported {
        String::new()
    } else {
        let diff = diff_renderings(&render_golden(&imported), &render_golden(&reimported), "import", "re-import");
        if diff.is_empty() {
            format!(
                "the DOM metadata differs (import: sequence {}, next node {}; re-import: sequence {}, next node {})",
                imported.seq(),
                imported.next_id(),
                reimported.seq(),
                reimported.next_id()
            )
        } else {
            diff
        }
    };
    if imported != reimported {
        violations.push(Violation::new(Rule::Reimport, None, "importing the export gives a different DOM"));
    }
    Ok(RoundTrip { native: native.to_owned(), imported, patches, violations, export_diffs, reimport_diff })
}
