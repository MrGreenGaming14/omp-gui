//! Fixtures shared by the updater's integration tests and the app's updater
//! tests (which include this file by path).
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// `path` spelled for a shell script: forward slashes, which Git Bash needs
/// on Windows and every Unix shell accepts.
pub fn sh_path(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// Writes an `omp` stand-in into `dir` and returns its path.
///
/// `--version` prints `omp/9.0.0`; any other call records its arguments, one
/// per line, in `dir/args.txt` and prints the contents of `dir/review.json`.
/// Unix gets a shell script. Windows gets a native executable, compiled once
/// per test process from `fixtures/omp_stub.rs`, because the changelog
/// prompt is a multi-line argument that a `.cmd` script cannot receive.
pub fn write_omp_stub(dir: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        let path = dir.join("omp");
        loa_exec::write_executable(
            &path,
            "#!/bin/sh\n\
             here=$(dirname \"$0\")\n\
             if [ \"$1\" = --version ]; then echo omp/9.0.0; exit 0; fi\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"$here/args.txt\"\n\
             cat \"$here/review.json\"\n",
        )
        .unwrap();
        path
    }
    #[cfg(not(unix))]
    {
        let path = dir.join("omp.exe");
        std::fs::copy(compiled_omp_stub(), &path).unwrap();
        path
    }
}

/// The `omp` stand-in executable, compiled on first use.
#[cfg(not(unix))]
fn compiled_omp_stub() -> &'static Path {
    use std::sync::LazyLock;
    static STUB: LazyLock<PathBuf> = LazyLock::new(|| {
        let dir = tempfile::tempdir().unwrap().keep();
        let source = dir.join("omp_stub.rs");
        std::fs::write(&source, include_str!("../fixtures/omp_stub.rs")).unwrap();
        let out = dir.join("omp-stub.exe");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let status = std::process::Command::new(rustc)
            .args(["--edition", "2021", "--crate-name", "omp_stub", "-o"])
            .arg(&out)
            .arg(&source)
            .status()
            .expect("run rustc to build the omp stand-in");
        assert!(status.success(), "rustc failed to build the omp stand-in: {status}");
        out
    });
    &STUB
}
