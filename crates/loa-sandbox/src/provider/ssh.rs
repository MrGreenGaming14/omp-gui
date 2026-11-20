//! Runs the stub on an SSH host. The host's `ssh` client does the connection,
//! so keys stay in `ssh-agent` and `~/.ssh/config` applies (aliases,
//! `ProxyJump`, ports). The stub is uploaded over the same command channel;
//! the workspace is copied in and out by [`crate::sync`].
use super::serial::command_channel;
use super::{Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::{Result, SandboxError};
use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

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

pub(super) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// An `ssh` invocation that never prompts: the app has no terminal to answer on.
fn ssh(program: &PathBuf, port: Option<u16>, host: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.arg("-T").arg("-o").arg("BatchMode=yes");
    if let Some(port) = port {
        cmd.arg("-p").arg(port.to_string());
    }
    cmd.arg("--").arg(host);
    cmd
}

fn failed(what: &str, output: &std::process::Output) -> SandboxError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    SandboxError::Provider { provider: "ssh", message: format!("{what} failed ({}): {}", output.status, stderr.trim()) }
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
        let stub = format!("{root}/loa-sandbox-stub");
        let binary = tokio::fs::read(&self.local_stub).await?;
        // One round trip creates the private root and receives the stub on stdin.
        let setup = format!("umask 077 && mkdir -p -- {root} && cat > {stub} && chmod 700 -- {stub}", root = quote(&root), stub = quote(&stub));
        let mut upload = ssh(&self.ssh, self.port, host).arg(setup)
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true).spawn()?;
        let mut stdin = upload.stdin.take().expect("piped stdin");
        let written = stdin.write_all(&binary).await;
        drop(stdin);
        let output = upload.wait_with_output().await?;
        if !output.status.success() || written.is_err() {
            let _ = cleanup(&self.ssh, self.port, host, &root).await;
            return Err(failed("stub upload", &output));
        }
        let mut cmd = ssh(&self.ssh, self.port, host);
        cmd.arg(format!("exec {} {}", quote(&stub), quote(&root)));
        let process = match command_channel(cmd, WorkspaceMode::Copied) {
            Ok(process) => process,
            Err(e) => { let _ = cleanup(&self.ssh, self.port, host, &root).await; return Err(e); }
        };
        Ok(Box::new(SshSandbox { process: Some(process), ssh: self.ssh.clone(), port: self.port, host: host.into(), root }))
    }
}

async fn cleanup(program: &PathBuf, port: Option<u16>, host: &str, root: &str) -> Result<()> {
    let output = ssh(program, port, host).arg(format!("rm -rf -- {}", quote(root))).stdin(Stdio::null()).output().await?;
    if !output.status.success() { return Err(failed("remote cleanup", &output)); }
    Ok(())
}

struct SshSandbox {
    process: Option<super::serial::ProcessSandbox>,
    ssh: PathBuf,
    port: Option<u16>,
    host: String,
    root: String,
}

impl Drop for SshSandbox {
    fn drop(&mut self) {
        if self.process.is_some() {
            let mut cmd = std::process::Command::new(&self.ssh);
            cmd.arg("-T").arg("-o").arg("BatchMode=yes");
            if let Some(port) = self.port { cmd.arg("-p").arg(port.to_string()); }
            let _ = cmd.arg("--").arg(&self.host).arg(format!("rm -rf -- {}", quote(&self.root)))
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }
}

#[async_trait]
impl Sandbox for SshSandbox {
    fn take_channel(&mut self) -> Option<super::Channel> { self.process.as_mut().and_then(|p| p.take_channel()) }
    fn workspace_mode(&self) -> WorkspaceMode { WorkspaceMode::Copied }
    async fn stop(mut self: Box<Self>) -> Result<()> {
        if let Some(process) = self.process.take() { Box::new(process).stop().await?; }
        cleanup(&self.ssh, self.port, &self.host, &self.root).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::SandboxOptions;
    #[cfg(unix)]
    use tokio::io::AsyncReadExt;

    #[test]
    fn shell_quote_handles_spaces_and_apostrophes() {
        assert_eq!(quote("a b'c"), "'a b'\\''c'");
    }

    #[cfg(unix)]
    fn script(path: &std::path::Path, body: &str) {
        loa_exec::write_executable(path, body).unwrap();
    }

    /// A fake `ssh` runs the remote command locally after `-- <host>`, and
    /// records the host it was asked for.
    #[cfg(unix)]
    #[tokio::test]
    async fn uploads_the_stub_runs_it_in_a_private_root_and_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let fake_ssh = dir.path().join("ssh");
        script(&fake_ssh, &format!("#!/bin/sh\nwhile [ \"$1\" != -- ]; do shift; done\nshift\necho \"$1\" >> {}/hosts\nshift\nexec sh -c \"$1\"\n", dir.path().display()));
        let stub = dir.path().join("stub");
        script(&stub, "#!/bin/sh\nprintf 'stub in %s' \"$1\"\n");
        let remote = dir.path().join("remote");
        let mut provider = SshProvider::new(&stub, vec!["builder".into()]);
        provider.ssh = fake_ssh;
        provider.remote_root = remote.display().to_string();
        let spec = SandboxSpec { name: "loa-sbx-7".into(), workspace: "/unused".into(), env: vec![], options: SandboxOptions::default() };
        let mut sandbox = provider.start(&spec).await.unwrap();
        assert_eq!(sandbox.workspace_mode(), WorkspaceMode::Copied);
        let mut output = String::new();
        sandbox.take_channel().unwrap().reader.read_to_string(&mut output).await.unwrap();
        let root = remote.join("loa-sbx-7");
        assert_eq!(output, format!("stub in {}", root.display()));
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(root.join("loa-sandbox-stub")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        sandbox.stop().await.unwrap();
        assert!(!root.exists(), "the remote root is removed");
        let hosts = std::fs::read_to_string(dir.path().join("hosts")).unwrap();
        assert!(hosts.lines().all(|host| host == "builder"), "{hosts}");
    }

    #[tokio::test]
    async fn needs_a_host_when_several_are_known() {
        let provider = SshProvider::new("/bin/true", vec!["a".into(), "b".into()]);
        let spec = SandboxSpec { name: "x".into(), workspace: "/unused".into(), env: vec![], options: SandboxOptions::default() };
        let error = provider.start(&spec).await.err().unwrap();
        assert!(error.to_string().contains("a valid host is required"), "{error}");
        assert_eq!(provider.describe(), "an SSH host; set options.host to one of a, b");
    }
}
