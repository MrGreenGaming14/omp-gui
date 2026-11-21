//! Turns a gate report into swap or hold, and applies the user's choice for a held update.

use crate::state::UpdaterState;
use loa_settings::updates::{GateReport, HeldUpdate, HoldChoice};
use semver::Version;

/// Records `report`: a clean one becomes a pending swap, a harmful one a hold. Returns whether it was clean.
pub fn record(state: &mut UpdaterState, report: &GateReport, now_unix: u64) -> bool {
    let product = report.product.clone();
    if report.is_clean() {
        state.held.remove(&product);
        state.pending_swap.insert(product, report.to.clone());
        return true;
    }
    if state.pending_swap.get(&product) == Some(&report.to) {
        state.pending_swap.remove(&product);
    }
    state.held.insert(product, HeldUpdate { version: report.to.clone(), report: report.clone(), held_at_unix: now_unix });
    false
}

/// Applies `choice` to the update held for `product`; returns the version to swap to now.
pub fn apply_choice(state: &mut UpdaterState, product: &str, current: &Version, choice: HoldChoice) -> Option<Version> {
    if choice == HoldChoice::PinCurrent {
        state.held.remove(product);
        state.pending_swap.remove(product);
        state.pins.insert(product.to_owned(), current.clone());
        return None;
    }
    let held = state.held.remove(product)?;
    if choice == HoldChoice::UpdateAnyway {
        // An earlier pin would otherwise keep offering only the old version.
        state.pins.remove(product);
        state.pending_swap.insert(product.to_owned(), held.version.clone());
        return Some(held.version);
    }
    if state.pending_swap.get(product) == Some(&held.version) {
        state.pending_swap.remove(product);
    }
    if state.waits.get(product).is_none_or(|waited| held.version > *waited) {
        state.waits.insert(product.to_owned(), held.version);
    }
    None
}

/// Whether `version` may be offered: not against another pinned version, nor at or below a wait.
pub fn offer_allowed(state: &UpdaterState, product: &str, version: &Version) -> bool {
    state.pins.get(product).is_none_or(|pin| pin == version) && state.waits.get(product).is_none_or(|waited| version > waited)
}
