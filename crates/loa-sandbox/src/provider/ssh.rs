//! Runs the stub on an SSH host. The host's `ssh` client does the connection,
//! so keys stay in `ssh-agent` and `~/.ssh/config` applies (aliases,
//! `ProxyJump`, ports). The stub is uploaded over the same command channel;
//! the workspace is copied in and out by [`crate::sync`].

use super::{Availability, Machine, ProviderFactory, ProviderStatus, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, Teardown, WorkspaceMode, process, status};
use crate::service::SandboxConfig;
use crate::{Result, SandboxError};
use async_trait::async_trait;
use loa_util::exec::shell_quote;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

pub struct SshProvider {
    pub ssh: PathBuf,
    /// Hosts the agent may pick from (aliases or `user@host`). With exactly
    /// one, calls may leave `options.host` out.
    pub hosts: Vec<String>,
    /// Overrides the port from `~/.ssh/config`.
    pub port: Option<u16>,
    /// The stub binary to upload. It must run on the remote host's OS and CPU.
    pub local_stub: PathBuf,
    pub remote_root: String,
}

impl SshProvider {
    pub fn new(local_stub: impl Into<PathBuf>, hosts: Vec<String>) -> Self {
        Self { ssh: "ssh".into(), hosts, port: None, local_stub: local_stub.into(), remote_root: "/tmp/loa-sandbox".into() }
    }
}

/// Runs a remote command on one host without ever prompting: the app has no terminal to answer on.
#[derive(Clone)]
struct Remote { ssh: PathBuf, port: Option<u16>, host: String, script: String }

impl Teardown for Remote {
    fn command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.ssh);
        cmd.args(["-T", "-o", "BatchMode=yes"]);
        if let Some(port) = self.port {
            cmd.arg("-p").arg(port.to_string());
        }
        cmd.args(["--", &self.host, &self.script]);
        cmd
    }
}

#[async_trait]
impl SandboxProvider for SshProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Ssh }

    fn describe(&self) -> String {
        match self.hosts.as_slice() {
            [] => "an SSH host; set options.host".into(),
            [only] => format!("the SSH host {only} (options.host may name another)"),
            hosts => format!("an SSH host; set options.host to one of {}", hosts.join(", ")),
        }
    }

    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let host = match (spec.options.host.as_deref(), self.hosts.as_slice()) {
            (Some(host), _) => host,
            (None, [only]) => only.as_str(),
            (None, _) => "",
        };
        if host.is_empty() || host.starts_with('-') {
            return Err(SandboxError::Provider { provider: "ssh", message: "a valid host is required".into() });
        }
        let root = format!("{}/{}", self.remote_root.trim_end_matches('/'), spec.name);
        let (q_root, q_stub) = (shell_quote(&root), shell_quote(&format!("{root}/loa-sandbox-stub")));
        let remote = |script: String| Remote { ssh: self.ssh.clone(), port: self.port, host: host.into(), script };
        let cleanup = remote(format!("rm -rf -- {q_root}"));
        let binary = tokio::fs::read(&self.local_stub).await?;
        // One round trip creates the private root and receives the stub on stdin.
        let setup = remote(format!("umask 077 && mkdir -p -- {q_root} && cat > {q_stub} && chmod 700 -- {q_stub}"));
        let mut upload = tokio::process::Command::from(setup.command())
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true).spawn()?;
        let written = upload.stdin.take().expect("piped stdin").write_all(&binary).await;
        let output = upload.wait_with_output().await?;
        if !output.status.success() || written.is_err() {
            let _ = tokio::process::Command::from(cleanup.command()).output().await;
            let message = format!("stub upload failed ({}): {}", output.status, String::from_utf8_lossy(&output.stderr).trim());
            return Err(SandboxError::Provider { provider: "ssh", message });
        }
        let run = remote(format!("exec {q_stub} {q_root}")).command();
        Ok(Box::new(process("ssh", run.into(), WorkspaceMode::Copied, Some(Box::new(cleanup)))?))
    }
}

pub struct Factory;

impl Factory {
    /// The hosts of the settings, else the aliases of the SSH config.
    fn hosts(machine: &Machine, config: &SandboxConfig) -> Vec<String> {
        match (&config.ssh_hosts[..], &machine.ssh_config) {
            ([], Some(file)) => loa_util::ssh::aliases(file),
            (hosts, _) => hosts.to_vec(),
        }
    }
}

#[async_trait]
impl ProviderFactory for Factory {
    fn kind(&self) -> SandboxKind { SandboxKind::Ssh }
    async fn probe(&self, machine: &Machine, config: &SandboxConfig) -> ProviderStatus {
        use Availability::*;
        match (machine.which("ssh"), &machine.stub, &Self::hosts(machine, config)[..]) {
            (None, ..) => status(self.kind(), Unavailable, "No ssh client is installed"),
            (Some(_), None, _) => status(self.kind(), Unavailable, super::NO_STUB),
            (Some(_), Some(_), []) => status(self.kind(), NotConfigured, "Add a host to ~/.ssh/config or the sandbox settings"),
            (Some(_), Some(_), hosts) => status(self.kind(), Ready, format!(
                "{}; hosts must run {} on {}", hosts.join(", "), std::env::consts::OS, std::env::consts::ARCH
            )),
        }
    }
    fn build(&self, machine: &Machine, config: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>> {
        let mut provider = SshProvider::new(machine.stub.as_ref()?, Self::hosts(machine, config));
        provider.ssh = machine.which("ssh")?;
        Some(Arc::new(provider))
    }
}
