//! Race-free writing and launching of executables.
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
//!
//! Production code that runs binaries it wrote itself (staged updates) cannot
//! avoid the writable descriptor; it closes and syncs the file before running
//! it and launches through [`retry_busy`], which waits out the short window
//! in which a concurrently forked child still holds it.

use std::io;
use std::path::Path;
use std::time::Duration;

/// Calls `spawn` and, while it fails with `ETXTBSY` ("Text file busy"),
/// calls it again after a short doubling backoff (2 ms up to 50 ms, about
/// 0.4 s in total). A forked child that inherited a writable descriptor
/// drops it when it execs, so the window is tiny. Any other error, or a
/// file still busy when the attempts run out, is returned as is.
pub fn retry_busy<T>(mut spawn: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    const ATTEMPTS: u32 = 12;
    let mut delay = Duration::from_millis(2);
    for _ in 1..ATTEMPTS {
        match spawn() {
            Err(err) if err.kind() == io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(delay);
                delay = (delay * 2).min(Duration::from_millis(50));
            }
            other => return other,
        }
    }
    spawn()
}

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

    /// Writes a script through a descriptor that stays open for writing, the
    /// state a concurrently forked child leaves a freshly written file in.
    fn open_for_writing(path: &Path) -> std::fs::File {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o755).open(path).unwrap();
        file.write_all(b"#!/bin/sh\necho ran\n").unwrap();
        file
    }

    #[test]
    fn retry_busy_runs_the_file_once_the_writer_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("staged");
        let writer = open_for_writing(&path);
        let busy = Command::new(&path).output().unwrap_err();
        assert_eq!(busy.kind(), io::ErrorKind::ExecutableFileBusy, "{busy}");

        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            drop(writer);
        });
        let mut attempts = 0;
        let out = retry_busy(|| {
            attempts += 1;
            Command::new(&path).output()
        })
        .unwrap();
        closer.join().unwrap();
        assert_eq!(out.stdout, b"ran\n");
        assert!(attempts > 1, "the first attempt must have hit ETXTBSY");
    }

    #[test]
    fn retry_busy_gives_up_and_passes_other_errors_through() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("staged");
        let _writer = open_for_writing(&path);
        let mut attempts = 0;
        let err = retry_busy(|| {
            attempts += 1;
            Command::new(&path).output()
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::ExecutableFileBusy);
        assert_eq!(attempts, 12);

        let mut attempts = 0;
        let err = retry_busy(|| {
            attempts += 1;
            Command::new(dir.path().join("missing")).output()
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts, 1);
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
