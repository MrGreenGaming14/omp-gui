//! Local shells on a PTY, exposed as a [`PtyChannel`].
//!
//! Output is read on a dedicated thread and handed over as [`Bytes`] chunks
//! with no further copies. A second thread waits on the child so it is always
//! reaped, and reports its exit status after the output ends.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::{mpsc, oneshot};

use crate::channel::{ChannelRead, ChannelReader, ChannelWriter, PtyChannel};
use crate::{Error, GridSize, Result};

/// Bytes requested from the PTY per read.
const READ_CHUNK: usize = 64 * 1024;
/// Chunks buffered between the read thread and the pane. When the pane falls
/// behind, the read thread blocks and the kernel applies backpressure to the
/// program.
const READ_QUEUE: usize = 64;
/// `EIO`, returned by a PTY master once every slave descriptor is closed.
const EIO: i32 = 5;

/// What to run in a local terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalShell {
    /// Program to run. `None` runs the user's login shell (`$SHELL`, or the
    /// password database entry) as a login shell.
    pub program: Option<PathBuf>,
    /// Arguments passed to `program`. Ignored for the login shell.
    pub args: Vec<OsString>,
    /// Working directory the program starts in.
    pub cwd: PathBuf,
    /// Extra environment variables on top of the app's environment.
    pub env: Vec<(OsString, OsString)>,
}

impl LocalShell {
    /// The user's login shell, as named by `$SHELL`, started in `cwd`.
    pub fn login_shell(cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: None,
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
        }
    }

    /// A specific program with arguments, started in `cwd`.
    pub fn program(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
        cwd: impl Into<PathBuf>,
    ) -> Self {
        Self {
            program: Some(program.into()),
            args: args.into_iter().map(Into::into).collect(),
            cwd: cwd.into(),
            env: Vec::new(),
        }
    }
}

impl From<GridSize> for PtySize {
    fn from(size: GridSize) -> Self {
        PtySize {
            rows: size.rows,
            cols: size.cols,
            pixel_width: size.cell_width.saturating_mul(size.cols),
            pixel_height: size.cell_height.saturating_mul(size.rows),
        }
    }
}

fn spawn_error(error: impl std::fmt::Display) -> Error {
    Error::Spawn(error.to_string())
}

/// Starts `shell` on a new PTY of the given size.
///
/// Spawning is synchronous and quick (fork and exec). The returned channel's
/// methods must be awaited inside a tokio runtime.
pub fn spawn(shell: &LocalShell, size: GridSize) -> Result<PtyChannel> {
    if !shell.cwd.is_dir() {
        return Err(Error::NotADirectory(shell.cwd.clone()));
    }
    let pair = native_pty_system()
        .openpty(size.into())
        .map_err(spawn_error)?;

    let mut command = match &shell.program {
        Some(program) => {
            let mut command = CommandBuilder::new(program);
            command.args(&shell.args);
            command
        }
        None => CommandBuilder::new_default_prog(),
    };
    command.cwd(&shell.cwd);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    for (key, value) in &shell.env {
        command.env(key, value);
    }

    let mut child = pair.slave.spawn_command(command).map_err(spawn_error)?;
    // The slave end must be closed here, or the master never sees EOF when
    // the shell exits.
    drop(pair.slave);
    let mut killer = child.clone_killer();

    let started = (|| -> Result<_> {
        let reader = pair.master.try_clone_reader().map_err(spawn_error)?;
        let writer = pair.master.take_writer().map_err(spawn_error)?;
        let (data_tx, data_rx) = mpsc::channel(READ_QUEUE);
        let (exit_tx, exit_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("loa-pty-read".into())
            .spawn(move || read_loop(reader, data_tx))?;
        Ok((writer, data_rx, exit_tx, exit_rx))
    })();
    let (writer, data_rx, exit_tx, exit_rx) = match started {
        Ok(parts) => parts,
        Err(error) => {
            // Best effort: the child must not outlive a failed start.
            let _ = killer.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let waiter = std::thread::Builder::new()
        .name("loa-pty-wait".into())
        .spawn(move || {
            let code = child
                .wait()
                .ok()
                .filter(|status| status.signal().is_none())
                .map(|status| status.exit_code());
            let _ = exit_tx.send(code);
        });
    if let Err(error) = waiter {
        let _ = killer.kill();
        return Err(error.into());
    }

    Ok(PtyChannel {
        reader: Box::new(LocalReader {
            data: data_rx,
            exit: Some(exit_rx),
        }),
        writer: Box::new(LocalWriter {
            writer: Some(writer),
            master: Some(pair.master),
            killer: Some(killer),
        }),
    })
}

/// Reads the PTY master until EOF and forwards each chunk.
fn read_loop(mut reader: Box<dyn Read + Send>, tx: mpsc::Sender<io::Result<Bytes>>) {
    let mut buf = BytesMut::with_capacity(READ_CHUNK);
    loop {
        buf.reserve(READ_CHUNK);
        buf.resize(READ_CHUNK, 0);
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                buf.truncate(n);
                if tx.blocking_send(Ok(buf.split().freeze())).is_err() {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.raw_os_error() == Some(EIO) => break,
            Err(error) => {
                let _ = tx.blocking_send(Err(error));
                break;
            }
        }
    }
}

struct LocalReader {
    data: mpsc::Receiver<io::Result<Bytes>>,
    exit: Option<oneshot::Receiver<Option<u32>>>,
}

#[async_trait]
impl ChannelReader for LocalReader {
    async fn read(&mut self) -> Result<ChannelRead> {
        match self.data.recv().await {
            Some(Ok(bytes)) => Ok(ChannelRead::Data(bytes)),
            Some(Err(error)) => Err(error.into()),
            None => {
                let exit = self.exit.take().ok_or(Error::Closed)?;
                // A dropped sender means the wait failed; the child is gone
                // either way.
                let exit_code = exit.await.unwrap_or(None);
                Ok(ChannelRead::Closed { exit_code })
            }
        }
    }
}

struct LocalWriter {
    writer: Option<Box<dyn Write + Send>>,
    master: Option<Box<dyn MasterPty + Send>>,
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
}

impl LocalWriter {
    /// Hangs up on the child (SIGHUP, like closing a terminal window) and
    /// releases the master. The wait thread reaps the child.
    fn shutdown(&mut self) {
        self.writer = None;
        self.master = None;
        if let Some(mut killer) = self.killer.take() {
            // Fails only when the child already exited.
            let _ = killer.kill();
        }
    }
}

#[async_trait]
impl ChannelWriter for LocalWriter {
    async fn write(&mut self, bytes: Bytes) -> Result<()> {
        let mut writer = self.writer.take().ok_or(Error::Closed)?;
        // A full PTY input buffer blocks the write, so it runs off the
        // async executor.
        let (writer, result) = tokio::task::spawn_blocking(move || {
            let result = writer.write_all(&bytes).and_then(|()| writer.flush());
            (writer, result)
        })
        .await
        .map_err(io::Error::other)?;
        self.writer = Some(writer);
        Ok(result?)
    }

    async fn resize(&mut self, size: GridSize) -> Result<()> {
        let master = self.master.as_ref().ok_or(Error::Closed)?;
        master
            .resize(size.into())
            .map_err(|error| Error::Io(io::Error::other(error.to_string())))
    }

    async fn close(&mut self) -> Result<()> {
        self.shutdown();
        Ok(())
    }
}

impl Drop for LocalWriter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const LIMIT: Duration = Duration::from_secs(10);

    fn sh(cwd: &std::path::Path) -> LocalShell {
        let mut shell = LocalShell::program("/bin/sh", Vec::<OsString>::new(), cwd);
        shell.env.push(("PS1".into(), "$ ".into()));
        shell
    }

    /// Reads until the accumulated output contains `needle`.
    async fn read_until(reader: &mut dyn ChannelReader, out: &mut Vec<u8>, needle: &str) {
        let wait = async {
            while !String::from_utf8_lossy(out).contains(needle) {
                match reader.read().await.expect("read") {
                    ChannelRead::Data(bytes) => out.extend_from_slice(&bytes),
                    ChannelRead::Closed { .. } => panic!(
                        "closed before {needle:?}: {}",
                        String::from_utf8_lossy(out)
                    ),
                }
            }
        };
        tokio::time::timeout(LIMIT, wait).await.expect("output in time");
    }

    async fn read_to_close(reader: &mut dyn ChannelReader) -> Option<u32> {
        let wait = async {
            loop {
                if let ChannelRead::Closed { exit_code } = reader.read().await.expect("read") {
                    return exit_code;
                }
            }
        };
        tokio::time::timeout(LIMIT, wait).await.expect("closed in time")
    }

    #[tokio::test]
    async fn shell_starts_in_requested_cwd_and_runs_input() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let mut channel = spawn(&sh(&cwd), GridSize::new(80, 24)).unwrap();
        channel
            .writer
            .write(Bytes::from_static(b"pwd; echo done-$((40+2))\n"))
            .await
            .unwrap();
        let mut out = Vec::new();
        read_until(channel.reader.as_mut(), &mut out, "done-42").await;
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains(cwd.to_str().unwrap()), "{text}");
    }

    #[tokio::test]
    async fn resize_reaches_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let mut channel = spawn(&sh(dir.path()), GridSize::new(80, 24)).unwrap();
        channel.writer.resize(GridSize::new(132, 43)).await.unwrap();
        channel
            .writer
            .write(Bytes::from_static(b"stty size; echo end\n"))
            .await
            .unwrap();
        let mut out = Vec::new();
        read_until(channel.reader.as_mut(), &mut out, "43 132").await;
    }

    #[tokio::test]
    async fn exit_status_is_reported_after_output() {
        let dir = tempfile::tempdir().unwrap();
        let mut channel = spawn(&sh(dir.path()), GridSize::new(80, 24)).unwrap();
        channel
            .writer
            .write(Bytes::from_static(b"echo bye; exit 3\n"))
            .await
            .unwrap();
        let mut out = Vec::new();
        read_until(channel.reader.as_mut(), &mut out, "bye\r\n").await;
        assert_eq!(read_to_close(channel.reader.as_mut()).await, Some(3));
    }

    #[tokio::test]
    async fn close_hangs_up_a_busy_shell() {
        let dir = tempfile::tempdir().unwrap();
        let mut channel = spawn(&sh(dir.path()), GridSize::new(80, 24)).unwrap();
        channel
            .writer
            .write(Bytes::from_static(b"echo ready; read line\n"))
            .await
            .unwrap();
        let mut out = Vec::new();
        read_until(channel.reader.as_mut(), &mut out, "ready\r\n").await;
        channel.writer.close().await.unwrap();
        // Killed by SIGHUP, so there is no exit code, and the child was reaped.
        assert_eq!(read_to_close(channel.reader.as_mut()).await, None);
        assert!(matches!(
            channel.writer.write(Bytes::from_static(b"x")).await,
            Err(Error::Closed)
        ));
    }

    #[tokio::test]
    async fn dropping_the_writer_hangs_up_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let PtyChannel { mut reader, writer } =
            spawn(&sh(dir.path()), GridSize::new(80, 24)).unwrap();
        drop(writer);
        read_to_close(reader.as_mut()).await;
    }

    #[test]
    fn missing_cwd_is_rejected_before_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        let error = spawn(&sh(&missing), GridSize::default()).unwrap_err();
        assert!(matches!(error, Error::NotADirectory(path) if path == missing));
    }
}
