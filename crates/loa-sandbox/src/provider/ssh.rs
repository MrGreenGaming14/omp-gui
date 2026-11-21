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
    /// The stub binary to upload. It must run on the remote host's OS and CPU.
    pub local_stub: PathBuf,
}

/// Where runs get their private directories on the host.
const REMOTE_ROOT: &str = "/tmp/loa-sandbox";

/// Runs a remote command on one host without ever prompting: the app has no terminal to answer on.
#[derive(Clone)]
struct Remote { ssh: PathBuf, host: String, script: String }

impl Teardown for Remote {
    fn command(&self) -> std::process::Command {
        let mut cmd = std::process::Command::new(&self.ssh);
        cmd.args(["-T", "-o", "BatchMode=yes", "--", &self.host, &self.script]);
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
        // The stub sits beside the workspace root, so copying out never brings it back.
        let dir = format!("{REMOTE_ROOT}/{}", spec.name);
        let (q_dir, q_root, q_stub) = (shell_quote(&dir), shell_quote(&format!("{dir}/workspace")), shell_quote(&format!("{dir}/loa-sandbox-stub")));
        let remote = |script: String| Remote { ssh: self.ssh.clone(), host: host.into(), script };
        let cleanup = remote(format!("rm -rf -- {q_dir}"));
        let binary = tokio::fs::read(&self.local_stub).await?;
        // One round trip creates the private directory and receives the stub on stdin.
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
    fn hosts(config: &SandboxConfig) -> Vec<String> {
        match (&config.ssh_hosts[..], loa_util::fs::home()) {
            ([], Some(home)) => loa_util::ssh::aliases(&home.join(".ssh/config")),
            (hosts, _) => hosts.to_vec(),
        }
    }
}

#[async_trait]
impl ProviderFactory for Factory {
    fn kind(&self) -> SandboxKind { SandboxKind::Ssh }
    async fn probe(&self, machine: &Machine, config: &SandboxConfig) -> ProviderStatus {
        use Availability::*;
        match (cfg!(windows), machine.which("ssh"), &machine.stub, &Self::hosts(config)[..]) {
            (true, ..) => status(self.kind(), Unavailable, super::REMOTE_STUB_UNSUPPORTED),
            (_, None, ..) => status(self.kind(), Unavailable, "No ssh client is installed"),
            (_, Some(_), None, _) => status(self.kind(), Unavailable, super::NO_STUB),
            (_, Some(_), Some(_), []) => status(self.kind(), NotConfigured, "Add a host to ~/.ssh/config or the sandbox settings"),
            (_, Some(_), Some(_), hosts) => status(self.kind(), Ready, format!(
                "{}; hosts must run {} on {}", hosts.join(", "), std::env::consts::OS, std::env::consts::ARCH
            )),
        }
    }
    fn build(&self, machine: &Machine, config: &SandboxConfig) -> Option<Arc<dyn SandboxProvider>> {
        let (ssh, local_stub) = (machine.which("ssh").filter(|_| !cfg!(windows))?, machine.stub.clone()?);
        Some(Arc::new(SshProvider { ssh, hosts: Self::hosts(config), local_stub }))
    }
}
