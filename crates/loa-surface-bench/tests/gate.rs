//! Exercise the CLI exit code, not just the report's threshold comparison.

use std::process::Command;

#[test]
fn sixty_millisecond_render_delay_fails_the_gate_and_prints_budget_table() {
    let out = Command::new(env!("CARGO_BIN_EXE_latency-gate"))
        .args(["--iterations", "3", "--render-delay-ms", "60", "--json"])
        .output()
        .expect("gate executable must run");
    assert!(!out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("Total input to pixels"), "{stdout}");
    assert!(stdout.contains("\"p95_us\""), "{stdout}");
    assert!(String::from_utf8(out.stderr).unwrap().contains("exceeds"));
}
