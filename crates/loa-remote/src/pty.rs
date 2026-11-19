//! Interactive login shells for the terminal pane: a PTY channel on the
//! host's pooled connection, split so output is read while input is written.
//!
//! The user opens these, not the agent, so host limits and approvals do not
//! apply. Each open is still written to the audit log.

use std::collections::VecDeque;
use std::sync::Arc;

use bytes::Bytes;
use russh::client::Msg;
use russh::{ChannelMsg, ChannelReadHalf, ChannelWriteHalf};

use crate::connection::Session;
use crate::error::{Error, Result};

/// Terminal type announced to the remote side.
pub const TERM: &str = "xterm-256color";

/// One read from a shell channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellRead {
    /// Output of the remote program (stdout and stderr, as a PTY merges them).
    Data(Bytes),
    /// The shell ended; nothing more follows.
    Closed { exit_status: Option<u32> },
}

/// Output side of an interactive shell.
pub struct ShellReader {
    host: String,
    half: ChannelReadHalf,
    /// Output that arrived while the PTY and shell requests were answered.
    pending: VecDeque<Bytes>,
    exit_status: Option<u32>,
    done: bool,
    _session: Arc<Session>,
}

/// Input and control side of an interactive shell. Dropping it hangs up.
pub struct ShellWriter {
    host: String,
    half: Option<ChannelWriteHalf<Msg>>,
    _session: Arc<Session>,
}

/// An open interactive shell.
pub struct Shell {
    pub reader: ShellReader,
    pub writer: ShellWriter,
}

impl std::fmt::Debug for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shell").field("host", &self.reader.host).finish()
    }
}

/// Opens a PTY running the login shell on `session`.
pub(crate) async fn open(session: Arc<Session>, cols: u16, rows: u16) -> Result<Shell> {
    let host = session.host.clone();
    let ssh = |source| Error::Ssh { host: host.clone(), source };
    let channel = session.handle.channel_open_session().await.map_err(ssh)?;
    channel
        .request_pty(true, TERM, u32::from(cols), u32::from(rows), 0, 0, &[])
        .await
        .map_err(ssh)?;
    channel.request_shell(true).await.map_err(ssh)?;
    let (mut read, write) = channel.split();

    // Both requests were sent with want_reply: wait for their two answers,
    // keeping any output that races ahead of the second one.
    let mut pending = VecDeque::new();
    let mut answers = 0;
    while answers < 2 {
        match read.wait().await {
            Some(ChannelMsg::Success) => answers += 1,
            Some(ChannelMsg::Failure) => {
                let what = if answers == 0 { "a terminal" } else { "a shell" };
                return Err(Error::InvalidRequest(format!("{host} refused to open {what}")));
            }
            Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => pending.push_back(data),
            Some(ChannelMsg::Close | ChannelMsg::Eof) | None => {
                return Err(Error::InvalidRequest(format!("{host} closed the shell channel at once")));
            }
            Some(_) => {}
        }
    }
    Ok(Shell {
        reader: ShellReader {
            host: host.clone(),
            half: read,
            pending,
            exit_status: None,
            done: false,
            _session: session.clone(),
        },
        writer: ShellWriter { host, half: Some(write), _session: session },
    })
}

impl ShellReader {
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The next chunk of output. After [`ShellRead::Closed`] it keeps
    /// returning `Closed`.
    pub async fn read(&mut self) -> ShellRead {
        if let Some(data) = self.pending.pop_front() {
            return ShellRead::Data(data);
        }
        while !self.done {
            match self.half.wait().await {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    return ShellRead::Data(data);
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => self.exit_status = Some(exit_status),
                Some(ChannelMsg::Close) | None => self.done = true,
                Some(_) => {}
            }
        }
        ShellRead::Closed { exit_status: self.exit_status }
    }
}

impl ShellWriter {
    pub fn host(&self) -> &str {
        &self.host
    }

    fn half(&self) -> Result<&ChannelWriteHalf<Msg>> {
        self.half.as_ref().ok_or_else(|| Error::InvalidRequest(format!("the shell on {} is closed", self.host)))
    }

    fn ssh(&self, source: russh::Error) -> Error {
        Error::Ssh { host: self.host.clone(), source }
    }

    /// Sends keyboard input.
    pub async fn write(&mut self, bytes: Bytes) -> Result<()> {
        self.half()?.data_bytes(bytes).await.map_err(|e| self.ssh(e))
    }

    /// Tells the remote PTY its new size.
    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        self.half()?
            .window_change(u32::from(cols), u32::from(rows), 0, 0)
            .await
            .map_err(|e| self.ssh(e))
    }

    /// Hangs up: sends EOF and closes the channel.
    pub async fn close(&mut self) -> Result<()> {
        let Some(half) = self.half.take() else {
            return Ok(());
        };
        let _ = half.eof().await;
        half.close().await.map_err(|e| self.ssh(e))
    }
}

impl Drop for ShellWriter {
    fn drop(&mut self) {
        if let Some(half) = self.half.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                let _ = half.close().await;
            });
        }
    }
}
