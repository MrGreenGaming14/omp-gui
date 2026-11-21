//! Local programs on a PTY as a [`PtyChannel`]: a thread reads output chunks, a second thread reaps the child, and
//! the exit status is reported after the output ends. Chunks already queued when the pane reads are joined into one.

use crate::QueueReader;
use loa_view::terminal::port::{ChannelRead, ChannelWriter, Error, GridSize, PtyChannel, Result};
use async_trait::async_trait;
use bytes::Bytes;
use loa_util::LockExt;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{ffi::OsString, io::{self, Read, Write}, path::PathBuf, sync::{Arc, Mutex}};
use tokio::sync::mpsc;

const READ_CHUNK: usize = 64 * 1024;
/// Chunks buffered toward the pane; when it falls behind, the kernel throttles the program.
const READ_QUEUE: usize = 64;
/// `EIO`: a PTY master's reads fail with it once every slave descriptor is closed.
const EIO: i32 = 5;

/// `program` with `args` (the user's login shell when `None`) in `cwd`, with `env` added to the app's environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalShell { pub program: Option<PathBuf>, pub args: Vec<OsString>, pub cwd: PathBuf, pub env: Vec<(OsString, OsString)> }
impl LocalShell {
    pub fn login_shell(cwd: impl Into<PathBuf>) -> Self { Self { program: None, args: Vec::new(), cwd: cwd.into(), env: Vec::new() } }
}

/// The PTY master, shared by the writer and (on Windows) the wait thread; `None` once released.
type Master = Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>;

fn pty_size(s: GridSize) -> PtySize { PtySize { rows: s.rows, cols: s.cols, pixel_width: s.cell_width.saturating_mul(s.cols), pixel_height: s.cell_height.saturating_mul(s.rows) } }
fn spawn_error(e: impl std::fmt::Display) -> Error { Error::Spawn(e.to_string()) }

/// Starts `shell` on a new PTY; the channel must be driven inside a tokio runtime.
pub fn spawn(shell: &LocalShell, size: GridSize) -> Result<PtyChannel> {
    if !shell.cwd.is_dir() { return Err(Error::NotADirectory(shell.cwd.clone())) }
    let pair = native_pty_system().openpty(pty_size(size)).map_err(spawn_error)?;
    let (reader, writer) = (pair.master.try_clone_reader().map_err(spawn_error)?, pair.master.take_writer().map_err(spawn_error)?);
    let mut command = match &shell.program {
        Some(program) => CommandBuilder::from_argv([program.clone().into_os_string()].into_iter().chain(shell.args.iter().cloned()).collect()),
        None => CommandBuilder::new_default_prog(),
    };
    command.cwd(&shell.cwd);
    for (k, v) in [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")] { command.env(k, v) }
    shell.env.iter().for_each(|(k, v)| command.env(k, v));
    let mut child = pair.slave.spawn_command(command).map_err(spawn_error)?;
    // The master sees EOF on exit only once this slave end is closed.
    drop(pair.slave);
    let mut killer = child.clone_killer();
    let master: Master = Arc::new(Mutex::new(Some(pair.master)));
    #[cfg(windows)]
    let console = Arc::clone(&master);
    let (exit_tx, exit_rx) = std::sync::mpsc::channel();
    let wait = move || {
        let exit_code = child.wait().ok().filter(|s| s.signal().is_none()).map(|s| s.exit_code());
        // ConPTY keeps its output pipe open after the shell exits; closing
        // the pseudoconsole flushes it and lets the reader reach EOF.
        #[cfg(windows)]
        {
            let console = console.locked().take();
            drop(console);
        }
        let _ = exit_tx.send(exit_code);
    };
    std::thread::Builder::new().name("loa-pty-wait".into()).spawn(wait).inspect_err(|_| drop(killer.kill()))?;
    let (tx, rx) = mpsc::channel(READ_QUEUE);
    let read = move || {
        read_loop(reader, &tx);
        let exit_code = exit_rx.recv().ok().flatten();
        let _ = tx.blocking_send(Ok(ChannelRead::Closed { exit_code }));
    };
    std::thread::Builder::new().name("loa-pty".into()).spawn(read).inspect_err(|_| drop(killer.kill()))?;
    Ok(PtyChannel { reader: Box::new(QueueReader::new(rx)), writer: Box::new(LocalWriter { writer: Arc::new(Mutex::new(writer)), master, killer: Some(killer) }) })
}

fn read_loop(mut reader: Box<dyn Read + Send>, tx: &mpsc::Sender<Result<ChannelRead>>) {
    // Every read lands in one scratch buffer, zeroed once; each chunk is copied out at its real length.
    let mut scratch = vec![0u8; READ_CHUNK];
    loop {
        let chunk = match reader.read(&mut scratch) {
            Ok(0) => return,
            Ok(n) => Ok(ChannelRead::Data(Bytes::copy_from_slice(&scratch[..n]))),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.raw_os_error() == Some(EIO) => return,
            Err(e) => Err(e.into()),
        };
        let failed = chunk.is_err();
        if tx.blocking_send(chunk).is_err() || failed { return }
    }
}

/// A running child's PTY; closing or dropping it hangs up (SIGHUP, like closing a terminal window).
struct LocalWriter { writer: Arc<Mutex<Box<dyn Write + Send>>>, master: Master, killer: Option<Box<dyn ChildKiller + Send + Sync>> }
impl Drop for LocalWriter {
    fn drop(&mut self) {
        // Fails only when the child already exited.
        let _ = self.killer.take().map(|mut killer| killer.kill());
        // ClosePseudoConsole can wait for its output to drain, which needs the
        // reader this caller may be responsible for polling.
        #[cfg(windows)]
        if let Some(master) = self.master.locked().take() {
            let _ = std::thread::Builder::new().name("loa-pty-close".into()).spawn(move || drop(master));
        }
    }
}

#[async_trait]
impl ChannelWriter for LocalWriter {
    async fn write(&mut self, bytes: Bytes) -> Result<()> {
        let writer = Arc::clone(&self.writer);
        // A full PTY input buffer blocks, so the write runs off the executor.
        let write = move || { let mut w = writer.locked(); w.write_all(&bytes).and_then(|()| w.flush()) };
        Ok(tokio::task::spawn_blocking(write).await.map_err(io::Error::other)??)
    }
    async fn resize(&mut self, size: GridSize) -> Result<()> {
        let master = self.master.locked();
        master.as_ref().ok_or(Error::Closed)?.resize(pty_size(size)).map_err(|e| Error::Io(io::Error::other(e.to_string())))
    }
    async fn close(&mut self) -> Result<()> {
        let _ = self.killer.take().map(|mut killer| killer.kill());
        Ok(())
    }
}
