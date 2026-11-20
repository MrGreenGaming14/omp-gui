//! RPC protocol version check and the probe reading omp's `ready` frame.

use super::{CheckResult, GateCheck};
use serde::Deserialize;
use std::ops::RangeInclusive;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Asks an installed version which RPC protocol versions it offers.
#[async_trait::async_trait]
pub trait RpcProbe: Send + Sync {
    async fn protocol_versions(&self, executable: &Path) -> Result<Vec<u32>, String>;
}

/// Starts omp in RPC mode, reads its first stdout line (the `ready` frame)
/// and kills it. Sends nothing, so no session starts and no model is called.
#[derive(Clone, Debug)]
pub struct OmpReadyProbe {
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl OmpReadyProbe {
    /// Probe with the standard arguments `--mode rpc --no-session`.
    pub fn new(timeout: Duration) -> Self {
        Self {
            args: ["--mode", "rpc", "--no-session"].map(String::from).to_vec(),
            timeout,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReadyFrame {
    #[serde(rename = "type")]
    kind: String,
    protocol_version: Option<u32>,
    supported_protocol_versions: Option<Vec<u32>>,
}

/// Protocol versions advertised by a `ready` frame line.
fn parse_ready(line: &str) -> Result<Vec<u32>, String> {
    let frame: ReadyFrame =
        serde_json::from_str(line.trim()).map_err(|e| format!("first line is not a ready frame: {e}"))?;
    if frame.kind != "ready" {
        return Err(format!("first frame has type {:?}, expected \"ready\"", frame.kind));
    }
    match (frame.supported_protocol_versions, frame.protocol_version) {
        (Some(versions), _) if !versions.is_empty() => Ok(versions),
        (_, Some(version)) => Ok(vec![version]),
        _ => Err("ready frame advertises no protocol version".to_string()),
    }
}

#[async_trait::async_trait]
impl RpcProbe for OmpReadyProbe {
    async fn protocol_versions(&self, executable: &Path) -> Result<Vec<u32>, String> {
        let mut child = loa_exec::retry_busy(|| {
            Command::new(loa_exec::resolve(executable))
                .args(&self.args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
        })
        .map_err(|e| format!("cannot start {}: {e}", executable.display()))?;
        // Keep stdin open until the child is killed: closing it makes omp shut down.
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "child stdout was not captured".to_string())?;

        let mut line = String::new();
        let read = tokio::time::timeout(self.timeout, BufReader::new(stdout).read_line(&mut line)).await;

        // Best effort: the child may already have exited on its own.
        let _ = child.start_kill();
        let _ = child.wait().await;
        drop(stdin);

        match read {
            Err(_) => Err(format!("no ready frame within {:?}", self.timeout)),
            Ok(Err(e)) => Err(format!("reading stdout: {e}")),
            Ok(Ok(0)) => Err("process exited without writing a ready frame".to_string()),
            Ok(Ok(_)) => parse_ready(&line),
        }
    }
}

/// Harmful when none of the offered versions is in the adapter's range.
pub fn check_rpc_protocol(offered: &[u32], supported: RangeInclusive<u32>) -> CheckResult {
    let offered_list: Vec<String> = offered.iter().map(u32::to_string).collect();
    let mut details = vec![
        format!("offered: {}", offered_list.join(", ")),
        format!("supported: {}..={}", supported.start(), supported.end()),
    ];
    match offered.iter().copied().filter(|v| supported.contains(v)).max() {
        Some(version) => {
            details.push(format!("negotiated: {version}"));
            CheckResult::pass(GateCheck::RpcProtocol, details)
        }
        None => {
            details.push("negotiated: none".to_string());
            CheckResult::harmful(GateCheck::RpcProtocol, details)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gates::Verdict;

    #[test]
    fn rpc_ranges() {
        let ok = check_rpc_protocol(&[1, 2], 1..=1);
        assert_eq!(ok.verdict, Verdict::Pass);
        assert!(ok.details.contains(&"negotiated: 1".to_string()));
        let both = check_rpc_protocol(&[1, 2], 1..=3);
        assert!(both.details.contains(&"negotiated: 2".to_string()));
        assert_eq!(check_rpc_protocol(&[3], 1..=2).verdict, Verdict::Harmful);
        assert_eq!(check_rpc_protocol(&[], 1..=2).verdict, Verdict::Harmful);
    }

    #[test]
    fn ready_frame_parsing() {
        assert_eq!(
            parse_ready(r#"{"type":"ready","protocolVersion":1,"supportedProtocolVersions":[1,2]}"#),
            Ok(vec![1, 2])
        );
        assert_eq!(parse_ready(r#"{"type":"ready","protocolVersion":1}"#), Ok(vec![1]));
        assert!(parse_ready(r#"{"type":"event","protocolVersion":1}"#).is_err());
        assert!(parse_ready("Starting up...").is_err());
    }
}
