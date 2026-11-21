//! Launcher for an Xvfb virtual display.

use crate::browser::{BrowserError, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, Command};

/// How long Xvfb may take to report its display number.
const START_TIMEOUT: Duration = Duration::from_secs(15);

/// A running Xvfb server. Dropping it kills the server.
pub struct VirtualDisplay {
    child: Child,
    number: u32,
    socket: PathBuf,
}

impl VirtualDisplay {
    /// Starts `xvfb` with one `width` x `height` 24 bit screen. Xvfb picks a
    /// free display number and reports it on stdout (`-displayfd 1`) once it
    /// accepts connections; its socket is `socket_dir/X<n>`.
    pub async fn start(xvfb: &Path, socket_dir: &Path, width: u32, height: u32) -> Result<Self> {
        let mut child = Command::new(xvfb)
            .args(["-displayfd", "1", "-screen", "0"])
            .arg(format!("{width}x{height}x24"))
            .args(["-nolisten", "tcp", "-nocursor", "-noreset"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| BrowserError::Display(format!("cannot run {}: {e}", xvfb.display())))?;
        let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Err(BrowserError::Display("Xvfb pipes missing".into()));
        };

        let mut line = String::new();
        let read = tokio::time::timeout(START_TIMEOUT, BufReader::new(stdout).read_line(&mut line)).await;
        let number = match read {
            Ok(Ok(_)) => line.trim().parse::<u32>().ok(),
            Ok(Err(_)) | Err(_) => None,
        };
        let Some(number) = number else {
            let _ = child.start_kill();
            let _ = child.wait().await;
            let mut log = String::new();
            let _ = stderr.read_to_string(&mut log).await;
            return Err(BrowserError::Display(format!("Xvfb did not report a display: {}", log.trim())));
        };
        // Keep draining stderr so a chatty server never blocks on a full pipe.
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "xvfb", "{line}");
            }
        });
        Ok(Self { child, number, socket: socket_dir.join(format!("X{number}")) })
    }

    /// Value for `DISPLAY`, e.g. `:99`.
    pub fn display(&self) -> String {
        format!(":{}", self.number)
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Stops the server and waits for it to exit.
    pub async fn stop(mut self) -> Result<()> {
        self.child.start_kill()?;
        self.child.wait().await?;
        Ok(())
    }
}
