//! A native stand-in for `omp`, compiled by `tests/common/mod.rs` where a
//! shell script cannot serve: Windows launches `.cmd` scripts through
//! `cmd.exe`, which cannot carry the multi-line changelog prompt as an
//! argument.
//!
//! `--version` prints `omp/9.0.0`. Any other call records its arguments, one
//! per line, in `args.txt` next to this executable and prints the contents
//! of `review.json` next to it.

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--version") {
        println!("omp/9.0.0");
        return;
    }
    let exe = std::env::current_exe().expect("own path");
    let dir = exe.parent().expect("parent directory");
    std::fs::write(dir.join("args.txt"), args.join("\n")).expect("record arguments");
    let reply = std::fs::read(dir.join("review.json")).expect("read review.json");
    std::io::stdout().write_all(&reply).expect("write reply");
}
