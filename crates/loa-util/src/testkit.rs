//! Test helpers shared across crates (feature `testkit`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Polls `f` every 10 ms until it holds, panicking after `t`.
pub async fn eventually(mut f: impl FnMut() -> bool, t: Duration) {
    let deadline = Instant::now() + t;
    while !f() {
        assert!(Instant::now() < deadline, "condition not met within {t:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Writes an executable `/bin/sh` script `dir/name` running `sh` and returns the path to spawn
/// (on Windows its `.cmd` launcher, see [`crate::exec::write_executable`]).
pub fn fake_program(dir: &Path, name: &str, sh: &str) -> PathBuf {
    let path = dir.join(name);
    crate::exec::write_executable(&path, format!("#!/bin/sh\n{sh}\n")).expect("write fake program");
    crate::exec::resolve(&path)
}
