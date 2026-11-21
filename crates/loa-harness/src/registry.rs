//! The harness registry: one [`HarnessFactory`] per enabled harness, so the
//! app builds adapters and accounts by id instead of matching on names.

use loa_contracts::harness::HarnessFactory;
use std::sync::Arc;

/// Factories of the harnesses compiled in. Each harness module adds its own
/// as it moves onto the kit.
pub fn factories() -> Vec<Arc<dyn HarnessFactory>> {
    Vec::new()
}
