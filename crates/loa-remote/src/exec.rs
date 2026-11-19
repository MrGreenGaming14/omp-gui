//! Command execution for `remote_bash` (#98).

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

/// Captured result of a remote command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_status: Option<u32>,
    pub exit_signal: Option<String>,
    /// Bytes of stdout dropped past the output limit.
    pub stdout_dropped: u64,
    /// Bytes of stderr dropped past the output limit.
    pub stderr_dropped: u64,
    pub end: ExecEnd,
}

/// Keeps the first `cap` bytes, counts the rest.
fn push_capped(buf: &mut Vec<u8>, data: &[u8], cap: usize, dropped: &mut u64) {
    let room = cap.saturating_sub(buf.len());
    let take = room.min(data.len());
    buf.extend_from_slice(&data[..take]);
    *dropped += (data.len() - take) as u64;
}

fn into_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn signal_name(sig: Sig) -> String {
    match sig {
        Sig::Custom(name) => name,
        other => format!("{other:?}"),
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
    let ssh = |source| Error::Ssh {
        host: host.to_string(),
        source,
    };
    let mut channel = handle.channel_open_session().await.map_err(ssh)?;
    channel.exec(true, command).await.map_err(ssh)?;

    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let (mut stdout_dropped, mut stderr_dropped) = (0u64, 0u64);
    let (mut exit_status, mut exit_signal) = (None, None);
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let end = loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break ExecEnd::Cancelled,
            () = &mut deadline => break ExecEnd::TimedOut,
            msg = channel.wait() => match msg {
                None | Some(ChannelMsg::Close) => break ExecEnd::Exited,
                Some(ChannelMsg::Data { data }) => {
                    push_capped(&mut stdout, &data, max_output, &mut stdout_dropped);
                }
                Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                    push_capped(&mut stderr, &data, max_output, &mut stderr_dropped);
                }
                Some(ChannelMsg::ExitStatus { exit_status: s }) => exit_status = Some(s),
                Some(ChannelMsg::ExitSignal { signal_name: s, .. }) => {
                    exit_signal = Some(signal_name(s));
                }
                Some(ChannelMsg::Failure) => {
                    return Err(Error::InvalidRequest(format!(
                        "{host} refused to run the command"
                    )));
                }
                Some(_) => {}
            },
        }
    };
    if end != ExecEnd::Exited {
        // Ask the server to stop the process, then drop the channel. Servers
        // that ignore signals still end the session's process when the
        // channel closes.
        let _ = channel.signal(Sig::KILL).await;
        let _ = channel.close().await;
    }
    Ok(ExecOutput {
        stdout: into_string(stdout),
        stderr: into_string(stderr),
        exit_status,
        exit_signal,
        stdout_dropped,
        stderr_dropped,
        end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_cap_keeps_head_and_counts_tail() {
        let (mut buf, mut dropped) = (Vec::new(), 0);
        push_capped(&mut buf, b"hello ", 8, &mut dropped);
        push_capped(&mut buf, b"world", 8, &mut dropped);
        push_capped(&mut buf, b"!!", 8, &mut dropped);
        assert_eq!(buf, b"hello wo");
        assert_eq!(dropped, 5);
    }
}
