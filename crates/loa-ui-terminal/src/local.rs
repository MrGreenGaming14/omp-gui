//! Local programs on a PTY as a [`PtyChannel`]. A thread reads the output into [`Bytes`]
//! chunks, then reaps the child and reports its exit status.

use crate::{channel::{ChannelRead, ChannelWriter, PtyChannel}, Error, GridSize, Result};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use loa_util::LockExt;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{ffi::OsString, io::{self, Read, Write}, path::PathBuf, sync::{Arc, Mutex}};
use tokio::sync::mpsc;

const READ_CHUNK: usize = 64 * 1024;
/// Chunks buffered toward the pane; when it falls behind, the kernel throttles the program.
const READ_QUEUE: usize = 64;
/// `EIO`: a PTY master's reads fail with it once every slave descriptor is closed.
const EIO: i32 = 5;

/// What to run in a local terminal: `program` with `args`, or the user's login shell when `None`,
/// in `cwd` with `env` added to the app's environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalShell { pub program: Option<PathBuf>, pub args: Vec<OsString>, pub cwd: PathBuf, pub env: Vec<(OsString, OsString)> }

impl LocalShell {
    pub fn login_shell(cwd: impl Into<PathBuf>) -> Self { Self { program: None, args: Vec::new(), cwd: cwd.into(), env: Vec::new() } }
}

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
    let (tx, rx) = mpsc::channel(READ_QUEUE);
    let read = move || {
        read_loop(reader, &tx);
        let exit_code = child.wait().ok().filter(|s| s.signal().is_none()).map(|s| s.exit_code());
        let _ = tx.blocking_send(Ok(ChannelRead::Closed { exit_code }));
    };
    std::thread::Builder::new().name("loa-pty".into()).spawn(read).inspect_err(|_| drop(killer.kill()))?;
    Ok(PtyChannel { reader: Box::new(rx), writer: Box::new(LocalWriter(Some(Live { writer: Arc::new(Mutex::new(writer)), master: pair.master, killer }))) })
}

fn read_loop(mut reader: Box<dyn Read + Send>, tx: &mpsc::Sender<Result<ChannelRead>>) {
    let mut buf = BytesMut::with_capacity(READ_CHUNK);
    loop {
        buf.resize(READ_CHUNK, 0);
        let chunk = match reader.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => Ok(ChannelRead::Data(buf.split_to(n).freeze())),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.raw_os_error() == Some(EIO) => return,
            Err(e) => Err(e.into()),
        };
        let failed = chunk.is_err();
        if tx.blocking_send(chunk).is_err() || failed { return }
    }
}

/// A running child; dropping it hangs up (SIGHUP, like closing a terminal window).
struct Live { writer: Arc<Mutex<Box<dyn Write + Send>>>, master: Box<dyn MasterPty + Send>, killer: Box<dyn ChildKiller + Send + Sync> }

impl Drop for Live {
    fn drop(&mut self) { let _ = self.killer.kill(); }
}

struct LocalWriter(Option<Live>);

#[async_trait]
impl ChannelWriter for LocalWriter {
    async fn write(&mut self, bytes: Bytes) -> Result<()> {
        let writer = Arc::clone(&self.0.as_ref().ok_or(Error::Closed)?.writer);
        // A full PTY input buffer blocks, so the write runs off the executor.
        let write = move || {
            let mut w = writer.locked();
            w.write_all(&bytes).and_then(|()| w.flush())
        };
        Ok(tokio::task::spawn_blocking(write).await.map_err(io::Error::other)??)
    }
    async fn resize(&mut self, size: GridSize) -> Result<()> {
        self.0.as_ref().ok_or(Error::Closed)?.master.resize(pty_size(size)).map_err(|e| Error::Io(io::Error::other(e.to_string())))
    }
    async fn close(&mut self) -> Result<()> {
        self.0 = None;
        Ok(())
    }
}
