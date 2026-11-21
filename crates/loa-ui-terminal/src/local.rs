//! Local programs on a PTY as a [`PtyChannel`]. Output is read on a thread and handed over as
//! [`Bytes`] chunks; a second thread reaps the child and reports its exit status after the
//! output ends.

use crate::channel::{ChannelRead, ChannelReader, ChannelWriter, PtyChannel};
use crate::{Error, GridSize, Result};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use tokio::sync::{mpsc, oneshot};

const READ_CHUNK: usize = 64 * 1024;
/// Chunks buffered toward the pane; when it falls behind, the kernel throttles the program.
const READ_QUEUE: usize = 64;
/// `EIO`: a PTY master's reads fail with it once every slave descriptor is closed.
const EIO: i32 = 5;

/// What to run in a local terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalShell {
    /// `None` runs the user's login shell.
    pub program: Option<PathBuf>,
    /// Ignored for the login shell.
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// Added to the app's environment.
    pub env: Vec<(OsString, OsString)>,
}

impl LocalShell {
    pub fn login_shell(cwd: impl Into<PathBuf>) -> Self { Self { program: None, args: Vec::new(), cwd: cwd.into(), env: Vec::new() } }

    pub fn program(program: impl Into<PathBuf>, args: impl IntoIterator<Item = impl Into<OsString>>, cwd: impl Into<PathBuf>) -> Self {
        Self { program: Some(program.into()), args: args.into_iter().map(Into::into).collect(), ..Self::login_shell(cwd) }
    }
}

fn pty_size(s: GridSize) -> PtySize { PtySize { rows: s.rows, cols: s.cols, pixel_width: s.cell_width.saturating_mul(s.cols), pixel_height: s.cell_height.saturating_mul(s.rows) } }

fn spawn_error(e: impl std::fmt::Display) -> Error { Error::Spawn(e.to_string()) }

/// Starts `shell` on a new PTY; the channel must be driven inside a tokio runtime.
pub fn spawn(shell: &LocalShell, size: GridSize) -> Result<PtyChannel> {
    if !shell.cwd.is_dir() { return Err(Error::NotADirectory(shell.cwd.clone())) }
    let pair = native_pty_system().openpty(pty_size(size)).map_err(spawn_error)?;
    let mut command = match &shell.program {
        Some(program) => {
            let mut c = CommandBuilder::new(program);
            c.args(&shell.args);
            c
        }
        None => CommandBuilder::new_default_prog(),
    };
    command.cwd(&shell.cwd);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    shell.env.iter().for_each(|(k, v)| command.env(k, v));
    let mut child = pair.slave.spawn_command(command).map_err(spawn_error)?;
    // The master sees EOF on exit only once this slave end is closed.
    drop(pair.slave);
    let mut killer = child.clone_killer();
    let (data_tx, data) = mpsc::channel(READ_QUEUE);
    let (exit_tx, exit) = oneshot::channel();
    let started = (|| -> Result<_> {
        let (reader, writer) = (pair.master.try_clone_reader().map_err(spawn_error)?, pair.master.take_writer().map_err(spawn_error)?);
        std::thread::Builder::new().name("loa-pty-read".into()).spawn(move || read_loop(reader, data_tx))?;
        Ok(writer)
    })();
    let writer = match started {
        Ok(writer) => writer,
        Err(e) => {
            let _ = (killer.kill(), child.wait());
            return Err(e);
        }
    };
    let wait = move || {
        let code = child.wait().ok().filter(|s| s.signal().is_none()).map(|s| s.exit_code());
        let _ = exit_tx.send(code);
    };
    if let Err(e) = std::thread::Builder::new().name("loa-pty-wait".into()).spawn(wait) {
        let _ = killer.kill();
        return Err(e.into());
    }
    let writer = LocalWriter { writer: Some(writer), master: Some(pair.master), killer: Some(killer) };
    Ok(PtyChannel { reader: Box::new(LocalReader { data, exit: Some(exit) }), writer: Box::new(writer) })
}

fn read_loop(mut reader: Box<dyn Read + Send>, tx: mpsc::Sender<io::Result<Bytes>>) {
    let mut buf = BytesMut::with_capacity(READ_CHUNK);
    loop {
        buf.resize(READ_CHUNK, 0);
        let chunk = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => Ok(buf.split_to(n).freeze()),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.raw_os_error() == Some(EIO) => break,
            Err(e) => Err(e),
        };
        let failed = chunk.is_err();
        if tx.blocking_send(chunk).is_err() || failed { break }
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
            Some(chunk) => Ok(ChannelRead::Data(chunk?)),
            // A dropped sender means the wait failed; the child is gone either way.
            None => Ok(ChannelRead::Closed { exit_code: self.exit.take().ok_or(Error::Closed)?.await.unwrap_or(None) }),
        }
    }
}

struct LocalWriter {
    writer: Option<Box<dyn Write + Send>>,
    master: Option<Box<dyn MasterPty + Send>>,
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
}

impl LocalWriter {
    /// Hangs up on the child (SIGHUP, like closing a terminal window); the wait thread reaps it.
    fn shutdown(&mut self) {
        (self.writer, self.master) = (None, None);
        if let Some(mut k) = self.killer.take() { let _ = k.kill(); }
    }
}

#[async_trait]
impl ChannelWriter for LocalWriter {
    async fn write(&mut self, bytes: Bytes) -> Result<()> {
        let mut writer = self.writer.take().ok_or(Error::Closed)?;
        // A full PTY input buffer blocks, so the write runs off the executor.
        let (writer, result) = tokio::task::spawn_blocking(move || {
            let r = writer.write_all(&bytes).and_then(|()| writer.flush());
            (writer, r)
        }).await.map_err(io::Error::other)?;
        self.writer = Some(writer);
        Ok(result?)
    }

    async fn resize(&mut self, size: GridSize) -> Result<()> {
        self.master.as_ref().ok_or(Error::Closed)?.resize(pty_size(size)).map_err(|e| Error::Io(io::Error::other(e.to_string())))
    }

    async fn close(&mut self) -> Result<()> {
        self.shutdown();
        Ok(())
    }
}

impl Drop for LocalWriter {
    fn drop(&mut self) { self.shutdown() }
}
