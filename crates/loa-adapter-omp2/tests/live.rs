//! Availability against the `omp` installed on this machine.

use loa_adapter::{Adapter, AvailabilityStatus};
use loa_adapter_omp2::Omp2Adapter;

/// The installed omp (the TypeScript one) answers `--version` but refuses
/// omp2's DOM subscription, so the picker must show omp2 as incompatible,
/// not ready.
#[tokio::test]
#[ignore = "live: runs the installed `omp` in RPC mode (no model calls)"]
async fn installed_omp_is_reported_incompatible_with_omp2() {
    let availability = Omp2Adapter::new().availability().await;
    eprintln!("{availability:?}");
    assert_eq!(availability.status, AvailabilityStatus::Incompatible, "{availability:?}");
    assert!(availability.detail.as_deref().is_some_and(|d| d.contains("omp2's DOM")));
    assert!(availability.version.is_some());
}
