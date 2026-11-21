//! Command execution for `remote_bash`.

use std::time::Duration;

use russh::{ChannelMsg, Sig, client};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::connection::ClientHandler;
use crate::error::{Error, Result};

/// How a command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecEnd {
    /// The remote side closed the channel.
    Exited,
    TimedOut,
    Cancelled,
}

/// Captured result of a remote command; `*_dropped` count bytes past the output limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: Option<u32>,
    pub exit_signal: Option<String>,
    pub stdout_dropped: u64,
    pub stderr_dropped: u64,
    pub end: ExecEnd,
}

/// Captured bytes up to a cap, and how many were dropped past it.
#[derive(Default)]
struct Capped(Vec<u8>, u64);

impl Capped {
    fn push(&mut self, data: &[u8], cap: usize) {
        let take = cap.saturating_sub(self.0.len()).min(data.len());
        self.0.extend_from_slice(&data[..take]);
        self.1 += (data.len() - take) as u64;
    }

    fn text(self) -> String {
        String::from_utf8_lossy(&self.0).into_owned()
    }
}

/// Runs `command` on a new session channel of `handle`.
pub(crate) async fn run(
    handle: &client::Handle<ClientHandler>,
    host: &str,
    command: &str,
    max_output: usize,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<ExecOutput> {
    let ssh = |source| Error::Ssh { host: host.to_owned(), source };
    let mut channel = handle.channel_open_session().await.map_err(ssh)?;
    channel.exec(true, command).await.map_err(ssh)?;
    let (mut stdout, mut stderr, mut exit_status, mut exit_signal) = (Capped::default(), Capped::default(), None, None);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let end = loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break ExecEnd::Cancelled,
            () = &mut deadline => break ExecEnd::TimedOut,
            msg = channel.wait() => match msg {
                None | Some(ChannelMsg::Close) => break ExecEnd::Exited,
                Some(ChannelMsg::Data { data }) => stdout.push(&data, max_output),
                Some(ChannelMsg::ExtendedData { data, ext: 1 }) => stderr.push(&data, max_output),
                Some(ChannelMsg::ExitStatus { exit_status: s }) => exit_status = Some(s),
                Some(ChannelMsg::ExitSignal { signal_name: Sig::Custom(s), .. }) => exit_signal = Some(s),
                Some(ChannelMsg::ExitSignal { signal_name: s, .. }) => exit_signal = Some(format!("{s:?}")),
                Some(ChannelMsg::Failure) => return Err(Error::InvalidRequest(format!("{host} refused to run the command"))),
                Some(_) => {}
            },
        }
    };
    if end != ExecEnd::Exited {
        // Ask the server to stop the process; closing the channel ends it on servers that ignore signals.
        let _ = channel.signal(Sig::KILL).await;
        let _ = channel.close().await;
    }
    let (stdout_dropped, stderr_dropped) = (stdout.1, stderr.1);
    Ok(ExecOutput { stdout: stdout.text(), stderr: stderr.text(), exit_status, exit_signal, stdout_dropped, stderr_dropped, end })
}
