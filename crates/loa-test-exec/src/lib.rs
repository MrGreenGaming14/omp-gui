//! Fake executables for tests.
//!
//! A test that writes a script with `std::fs::write` and then runs it races
//! every other thread of the test binary: a child forked by another thread
//! between our `open` and `close` inherits the descriptor open for writing and
//! holds it until it execs, and exec of a file that is open for writing fails
//! with `ETXTBSY` ("Text file busy"). Renaming the file into place does not
//! help, the inode is the same.
//!
//! [`write_executable`] has a separate `sh` process create the file, so this
//! process never holds a writable descriptor for it that a fork could leak.
//! Once it returns, nothing has the file open for writing.

use std::io;
use std::path::Path;

/// Writes `contents` to `path` (created or truncated) with mode 0755.
#[cfg(unix)]
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut writer = Command::new("/bin/sh")
        .args(["-c", r#"cat > "$1" && chmod 755 "$1""#, "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()?;
    let mut stdin = writer.stdin.take().expect("stdin is piped");
    let written = stdin.write_all(contents.as_ref());
    drop(stdin);
    let status = writer.wait()?;
    written?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("writing {} failed: {status}", path.display())))
    }
}

/// Writes `contents` to `path`. Without fork there is no race to avoid.
#[cfg(not(unix))]
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    std::fs::write(path, contents)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    #[test]
    fn writes_bytes_and_makes_them_executable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool with space");
        write_executable(&path, "#!/bin/sh\nprintf 'ran %s' \"$1\"\n").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
        let out = Command::new(&path).arg("x").output().unwrap();
        assert_eq!(out.stdout, b"ran x");
        write_executable(&path, b"#!/bin/sh\nexit 3\n").unwrap();
        assert_eq!(Command::new(&path).status().unwrap().code(), Some(3));
    }

    /// Many threads write and immediately run fresh scripts while the others
    /// fork; none may see "Text file busy".
    #[test]
    fn fresh_executables_run_while_other_threads_fork() {
        let dir = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for thread in 0..16 {
                let dir = dir.path();
                scope.spawn(move || {
                    for round in 0..40 {
                        let path = dir.join(format!("t{thread}-{round}"));
                        write_executable(&path, format!("#!/bin/sh\necho {round}\n")).unwrap();
                        let out = Command::new(&path).output().unwrap_or_else(|e| panic!("{}: {e}", path.display()));
                        assert_eq!(String::from_utf8(out.stdout).unwrap(), format!("{round}\n"));
                    }
                });
            }
        });
    }
}
