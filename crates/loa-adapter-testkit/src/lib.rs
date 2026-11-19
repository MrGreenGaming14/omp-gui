//! Contract test suite shared by every harness adapter.
//!
//! - [`check_dom`]: the DOM conventions of `docs/ARCHITECTURE.md` on any DOM;
//!   [`check_history`] adds the status machines over the patches that built it
//! - [`check_capabilities`]: declared capabilities against behavior
//! - [`round_trip`]: import, export, format aware diff, re-import
//! - [`render_golden`] and [`Golden`]: golden fixtures, blessed with `LOA_BLESS=1`
//! - [`LiveDriver`]: send, answer, interrupt and wait on a live session,
//!   checking the conventions after each step
//! - [`ScriptedAdapter`]: a fake harness that plays scripts, the reference
//!   implementation of the conventions; [`scenarios`] holds ready-made scripts
//! - [`run_contract`]: all of the above for one adapter, for adapter tests and
//!   the updater's staged harness checks

pub mod caps;
pub mod check;
pub mod diff;
pub mod golden;
pub mod live;
pub mod roundtrip;
pub mod scenarios;
pub mod scripted;
pub mod suite;
mod violation;

pub use caps::{check_capabilities, check_produced};
pub use check::{Phase, check_dom, check_dom_in, check_history};
pub use diff::{FileDiff, Format, diff_bytes, diff_paths};
pub use golden::{BLESS_VAR, Golden, TIME_ATTRS, render_golden, render_golden_masked};
pub use live::{Action, LiveDriver, LiveReport};
pub use roundtrip::{RoundTrip, round_trip};
pub use scripted::{ScriptedAdapter, Step};
pub use suite::{Contract, ContractReport, LivePlan, run_contract};
pub use violation::{Rule, Violation};

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    #[error(transparent)]
    Adapter(#[from] loa_adapter::AdapterError),
    #[error("golden file {} doesn't exist; run with {}=1 to create it", path.display(), golden::BLESS_VAR)]
    GoldenMissing { path: PathBuf },
    #[error("golden file {} differs; run with {}=1 to accept:\n{diff}", path.display(), golden::BLESS_VAR)]
    GoldenMismatch { path: PathBuf, diff: String },
}

impl Error {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Error::Io { path: path.to_owned(), source }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
