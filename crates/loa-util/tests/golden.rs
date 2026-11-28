//! Writes the C++ golden fixtures for loa-util formats into `$LOA_GOLDEN_OUT/util`.
//! Run: `LOA_GOLDEN_OUT=$PWD/cpp/tests/golden cargo test -p loa-util --test golden -- --ignored`.

use std::path::PathBuf;

#[test]
#[ignore = "writes golden fixtures"]
fn write_golden() {
    let out = PathBuf::from(std::env::var("LOA_GOLDEN_OUT").expect("LOA_GOLDEN_OUT")).join("util");
    let iso: String = [0, 1, -1, 951_782_400_000, 1_709_251_199_123, 1_767_323_045_678, 253_402_300_799_999, -62_135_596_800_000]
        .iter()
        .map(|&ms| format!("{ms} {}\n", loa_util::time::format_iso_ms(ms)))
        .collect();
    std::fs::write(out.join("iso.txt"), iso).unwrap();
    let aliases = loa_util::ssh::aliases(&out.join("ssh/config"));
    std::fs::write(out.join("ssh/aliases.txt"), aliases.iter().map(|a| format!("{a}\n")).collect::<String>()).unwrap();
}
