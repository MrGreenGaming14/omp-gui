//! Docker CLI provider; mounts the host workspace and keeps the stub on stdio.
use super::serial::command_channel;
use super::{Channel, Sandbox, SandboxKind, SandboxProvider, SandboxSpec, WorkspaceMode};
use crate::{Result, SandboxError};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::process::Command;

/// Where a mounted host stub appears inside the container.
pub const MOUNTED_STUB: &str = "/opt/loa/loa-sandbox-stub";

/// Variables of the host the Docker CLI itself needs to find its daemon and
/// configuration. They reach the `docker` client process only, never the
/// container.
const CLIENT_ENV: &[&str] = &["PATH", "HOME", "XDG_RUNTIME_DIR", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY"];

/// How the container gets the stub.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockerStub {
    /// Bind mount this host binary read only at [`MOUNTED_STUB`]. Works with
    /// any image whose C library matches the host's.
    Mount(PathBuf),
    /// The image ships the stub at this path.
    InImage(String),
}

pub struct DockerProvider {
    pub docker: PathBuf,
    pub stub: DockerStub,
    /// Used when the call names no image.
    pub image: String,
    pub network: bool,
    pub memory_mib: Option<u32>,
    pub cpus: Option<u32>,
}

impl DockerProvider {
    pub fn new(image: impl Into<String>, stub: DockerStub) -> Self {
        Self { docker: "docker".into(), stub, image: image.into(), network: false, memory_mib: None, cpus: None }
    }

}

type ClientEnv = Vec<(&'static str, std::ffi::OsString)>;

fn client_env() -> ClientEnv {
    CLIENT_ENV.iter().filter_map(|key| std::env::var_os(key).map(|value| (*key, value))).collect()
}

fn docker_command(docker: &PathBuf, env: &ClientEnv) -> Command {
    let mut cmd = Command::new(docker);
    cmd.env_clear().envs(env.iter().map(|(key, value)| (key, value)));
    cmd
}

#[async_trait]
impl SandboxProvider for DockerProvider {
    fn kind(&self) -> SandboxKind { SandboxKind::Docker }
    fn describe(&self) -> String {
        format!("a Docker container (options.image, default {}; network {} unless options.network)", self.image, if self.network { "on" } else { "off" })
    }
    async fn start(&self, spec: &SandboxSpec) -> Result<Box<dyn Sandbox>> {
        let env = client_env();
        let mut cmd = docker_command(&self.docker, &env);
        cmd.arg("run").arg("--rm").arg("-i").arg("--name").arg(&spec.name)
            .arg("--network").arg(if spec.options.network.unwrap_or(self.network) { "bridge" } else { "none" })
            .arg("--mount").arg(format!("type=bind,src={},dst=/workspace", spec.workspace.display()))
            .arg("--workdir").arg("/workspace");
        let stub = match &self.stub {
            DockerStub::Mount(host) => {
                cmd.arg("--mount").arg(format!("type=bind,src={},dst={MOUNTED_STUB},readonly", host.display()));
                MOUNTED_STUB
            }
            DockerStub::InImage(path) => path.as_str(),
        };
        if let Some(memory) = spec.options.memory_mib.or(self.memory_mib) {
            cmd.arg("--memory").arg(format!("{memory}m"));
        }
        if let Some(cpus) = spec.options.cpus.or(self.cpus) {
            cmd.arg("--cpus").arg(cpus.to_string());
        }
        for (key, value) in &spec.env { cmd.arg("--env").arg(format!("{key}={value}")); }
        let image = spec.options.image.as_deref().filter(|image| !image.is_empty()).unwrap_or(&self.image);
        cmd.arg(image).arg(stub).arg("/workspace");
        let process = command_channel(cmd, WorkspaceMode::Shared)?;
        Ok(Box::new(DockerSandbox { process: Some(process), docker: self.docker.clone(), env, name: spec.name.clone() }))
    }
}

struct DockerSandbox {
    process: Option<super::serial::ProcessSandbox>,
    docker: PathBuf,
    env: ClientEnv,
    name: String,
}

impl Drop for DockerSandbox {
    fn drop(&mut self) {
        if self.process.is_some() {
            let _ = std::process::Command::new(&self.docker).env_clear().envs(self.env.iter().map(|(key, value)| (key, value)))
                .args(["rm", "-f", &self.name]).spawn();
        }
    }
}

#[async_trait]
impl Sandbox for DockerSandbox {
    fn take_channel(&mut self) -> Option<Channel> { self.process.as_mut().and_then(Sandbox::take_channel) }
    fn workspace_mode(&self) -> WorkspaceMode { WorkspaceMode::Shared }
    async fn stop(mut self: Box<Self>) -> Result<()> {
        let output = docker_command(&self.docker, &self.env).args(["rm", "-f", &self.name]).output().await?;
        // `--rm` may have removed it already when the stub exited.
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() && !stderr.contains("No such container") {
            return Err(SandboxError::Provider { provider: "docker", message: stderr.into_owned() });
        }
        if let Some(process) = self.process.take() { Box::new(process).stop().await?; }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::SandboxOptions;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::AsyncReadExt;

    /// A script stands in for the Docker CLI and prints its arguments, one per line.
    #[tokio::test]
    async fn runs_the_mounted_stub_in_an_isolated_container() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("docker");
        std::fs::write(&fake, "#!/bin/sh\nif [ \"$1\" = run ]; then printf '%s\\n' \"$@\"; fi\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut provider = DockerProvider::new("debian:stable-slim", DockerStub::Mount("/host/stub".into()));
        provider.docker = fake;
        let spec = SandboxSpec {
            name: "loa-sbx-1".into(),
            workspace: "/work".into(),
            env: vec![("A".into(), "1".into())],
            options: SandboxOptions { memory_mib: Some(256), ..Default::default() },
        };
        let mut sandbox = provider.start(&spec).await.unwrap();
        let mut args = String::new();
        sandbox.take_channel().unwrap().reader.read_to_string(&mut args).await.unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert_eq!(args, [
            "run", "--rm", "-i", "--name", "loa-sbx-1", "--network", "none",
            "--mount", "type=bind,src=/work,dst=/workspace", "--workdir", "/workspace",
            "--mount", "type=bind,src=/host/stub,dst=/opt/loa/loa-sandbox-stub,readonly",
            "--memory", "256m", "--env", "A=1", "debian:stable-slim", MOUNTED_STUB, "/workspace",
        ]);
        sandbox.stop().await.unwrap();
    }
}
