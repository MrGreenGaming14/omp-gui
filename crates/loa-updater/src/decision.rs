//! Turns a gate report into swap or hold, and applies the user's choice for
//! a held update.

use crate::gates::GateReport;
use crate::state::{HeldUpdate, UpdaterState};
use semver::Version;

/// What to do with a staged update after its gate checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Every check passed: swap at the next point between sessions.
    Swap,
    /// Something harmful: hold the update and show the report.
    Hold { report: GateReport },
}

/// The user's answer to a held update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldChoice {
    /// Swap to the held version despite the report.
    UpdateAnyway,
    /// Stay on the current version; nothing else is offered until unpinned.
    PinCurrent,
    /// Skip the held version and offer the next newer one.
    Wait,
}

/// Swap when the report is clean, hold otherwise.
pub fn decide(report: &GateReport) -> Decision {
    if report.is_clean() {
        Decision::Swap
    } else {
        Decision::Hold { report: report.clone() }
    }
}

/// Decides on `report` and records the outcome in `state`: a clean report
/// marks its version as pending swap and clears any hold; a harmful one
/// stores a hold and drops a pending swap of that same version.
pub fn record(state: &mut UpdaterState, report: &GateReport, now_unix: u64) -> Decision {
    let decision = decide(report);
    let product = report.product.clone();
    match &decision {
        Decision::Swap => {
            state.held.remove(&product);
            state.pending_swap.insert(product, report.to.clone());
        }
        Decision::Hold { report } => {
            if state.pending_swap.get(&product) == Some(&report.to) {
                state.pending_swap.remove(&product);
            }
            state.held.insert(
                product,
                HeldUpdate {
                    version: report.to.clone(),
                    report: report.clone(),
                    held_at_unix: now_unix,
                },
            );
        }
    }
    decision
}

/// Applies the user's `choice` for the update held for `product`, while
/// `current` is the active version. Returns the version to swap to now.
///
/// `UpdateAnyway` and `Wait` act on the held update and do nothing when
/// none is held. `PinCurrent` pins `current` either way.
pub fn apply_choice(
    state: &mut UpdaterState,
    product: &str,
    current: &Version,
    choice: HoldChoice,
) -> Option<Version> {
    match choice {
        HoldChoice::UpdateAnyway => {
            let held = state.held.remove(product)?;
            // Updating anyway overrides an earlier pin, which would
            // otherwise keep offering only the old version.
            state.pins.remove(product);
            state.pending_swap.insert(product.to_owned(), held.version.clone());
            Some(held.version)
        }
        HoldChoice::PinCurrent => {
            state.held.remove(product);
            state.pending_swap.remove(product);
            state.pins.insert(product.to_owned(), current.clone());
            None
        }
        HoldChoice::Wait => {
            let held = state.held.remove(product)?;
            if state.pending_swap.get(product) == Some(&held.version) {
                state.pending_swap.remove(product);
            }
            let newer_wait = match state.waits.get(product) {
                Some(waited) => held.version > *waited,
                None => true,
            };
            if newer_wait {
                state.waits.insert(product.to_owned(), held.version);
            }
            None
        }
    }
}

/// Whether `version` of `product` may be offered: not when the product is
/// pinned to another version, nor when it is the waited-on version or
/// older.
pub fn offer_allowed(state: &UpdaterState, product: &str, version: &Version) -> bool {
    if let Some(pin) = state.pins.get(product)
        && pin != version
    {
        return false;
    }
    match state.waits.get(product) {
        Some(waited) => version > waited,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gates::{CheckResult, GateCheck};

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn report(to: &str, harmful: bool) -> GateReport {
        let check = if harmful {
            CheckResult::harmful(GateCheck::ToolSchemas, vec!["tool `edit` removed".into()])
        } else {
            CheckResult::pass(GateCheck::ToolSchemas, vec![])
        };
        GateReport {
            product: "omp".into(),
            from: v("1.0.0"),
            to: v(to),
            checks: vec![CheckResult::pass(GateCheck::Changelog, vec![]), check],
        }
    }

    #[test]
    fn harmful_report_is_held_and_survives_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = UpdaterState::default();
        let decision = record(&mut state, &report("1.1.0", true), 1_700_000_000);
        assert!(matches!(decision, Decision::Hold { .. }));
        assert!(state.pending_swap.is_empty());
        state.save(dir.path()).unwrap();

        let loaded = UpdaterState::load(dir.path()).unwrap();
        let held = &loaded.held["omp"];
        assert_eq!(held.version, v("1.1.0"));
        assert_eq!(held.held_at_unix, 1_700_000_000);
        assert_eq!(held.report.harmful().count(), 1);
        assert_eq!(loaded, state);
    }

    #[test]
    fn missing_state_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(UpdaterState::load(dir.path()).unwrap(), UpdaterState::default());
    }

    #[test]
    fn clean_report_marks_pending_swap_and_clears_hold() {
        let mut state = UpdaterState::default();
        record(&mut state, &report("1.1.0", true), 1);
        assert_eq!(record(&mut state, &report("1.2.0", false), 2), Decision::Swap);
        assert!(state.held.is_empty());
        assert_eq!(state.pending_swap["omp"], v("1.2.0"));
    }

    #[test]
    fn update_anyway_swaps_held_version() {
        let mut state = UpdaterState::default();
        state.pins.insert("omp".into(), v("1.0.0"));
        record(&mut state, &report("1.1.0", true), 1);
        let to = apply_choice(&mut state, "omp", &v("1.0.0"), HoldChoice::UpdateAnyway);
        assert_eq!(to, Some(v("1.1.0")));
        assert!(state.held.is_empty());
        assert!(state.pins.is_empty());
        assert_eq!(state.pending_swap["omp"], v("1.1.0"));
    }

    #[test]
    fn pin_current_blocks_other_versions() {
        let mut state = UpdaterState::default();
        record(&mut state, &report("1.1.0", true), 1);
        assert_eq!(apply_choice(&mut state, "omp", &v("1.0.0"), HoldChoice::PinCurrent), None);
        assert!(state.held.is_empty());
        assert!(state.pending_swap.is_empty());
        assert!(!offer_allowed(&state, "omp", &v("1.1.0")));
        assert!(!offer_allowed(&state, "omp", &v("2.0.0")));
        assert!(offer_allowed(&state, "omp", &v("1.0.0")));
        assert!(offer_allowed(&state, "other", &v("2.0.0")));
    }

    #[test]
    fn wait_skips_held_version_until_a_newer_one() {
        let mut state = UpdaterState::default();
        record(&mut state, &report("1.1.0", true), 1);
        assert_eq!(apply_choice(&mut state, "omp", &v("1.0.0"), HoldChoice::Wait), None);
        assert!(state.held.is_empty());
        assert!(state.pending_swap.is_empty());
        assert!(!offer_allowed(&state, "omp", &v("1.1.0")));
        assert!(!offer_allowed(&state, "omp", &v("1.0.5")));
        assert!(offer_allowed(&state, "omp", &v("1.1.1")));
    }

    #[test]
    fn choices_without_a_hold_change_nothing_to_swap() {
        let mut state = UpdaterState::default();
        assert_eq!(apply_choice(&mut state, "omp", &v("1.0.0"), HoldChoice::UpdateAnyway), None);
        assert_eq!(apply_choice(&mut state, "omp", &v("1.0.0"), HoldChoice::Wait), None);
        assert_eq!(state, UpdaterState::default());
    }
}
