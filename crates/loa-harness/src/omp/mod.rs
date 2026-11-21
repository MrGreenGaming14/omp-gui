//! The omp harness: its RPC client, the session adapter on it, native
//! session files and one-shot model calls.

pub mod account;
pub mod validate;
pub mod oauth;
pub mod launch;
pub mod mapper;
pub mod native;
pub mod rpc;
mod session;

pub use launch::{MemoryMode, OmpAdapterConfig};
pub use native::OmpNative;
pub use session::OmpAdapter;

/// Harness id written to `<session harness>` and `<ext harness>`.
pub const HARNESS: &str = "omp";

use async_trait::async_trait;
use loa_contracts::Summarizer;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// `--config` overlay that switches omp's memory off for the run. `"off"` is
/// quoted: a YAML 1.1 parser reads a bare `off` as the boolean `false`.
pub const MEMORY_OFF: &str = "memory:\n  backend: \"off\"\n";

/// One model call through `omp -p`: the prompt goes in on stdin, the answer
/// is the printed text. The run uses a model role (`@smol` unless set),
/// saves no session, loads no tools, context files, rules, skills,
/// extensions or language servers, and turns omp's memory off, so these
/// calls never reach omp's own history.
#[derive(Clone, Debug)]
pub struct OneShot {
    executable: PathBuf,
    cwd: PathBuf,
    model: String,
    timeout: Duration,
}

impl OneShot {
    /// `executable` is the omp binary, `cwd` the directory it starts in.
    pub fn new(executable: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self { executable: executable.into(), cwd: cwd.into(), model: "@smol".to_owned(), timeout: Duration::from_secs(180) }
    }

    /// Model or role passed to `--model`, e.g. `@smol` or `provider/id`.
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// `model` when it is set, else the configured model.
    pub fn pick<'a>(&'a self, model: Option<&'a str>) -> &'a str {
        model.map(str::trim).filter(|m| !m.is_empty()).unwrap_or(&self.model)
    }

    fn args(&self, model: &str, overlay: &Path) -> Vec<OsString> {
        let flags = ["-p", "--mode", "text", "--no-session", "--no-tools", "--no-title", "--no-context-files", "--no-rules"];
        let more = ["--no-skills", "--no-extensions", "--no-lsp", "--model"];
        let mut args: Vec<OsString> = flags.into_iter().chain(more).map(Into::into).collect();
        args.extend([model.into(), "--config".into(), overlay.into(), "--cwd".into(), self.cwd.clone().into()]);
        args
    }

    /// Runs `prompt` on `model` and returns the printed answer.
    pub async fn ask(&self, model: &str, prompt: &str) -> Result<String, String> {
        let mut overlay = tempfile::Builder::new().prefix("loa-oneshot-").suffix(".yml").tempfile().map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut overlay, MEMORY_OFF.as_bytes()).map_err(|e| e.to_string())?;
        let mut cmd = tokio::process::Command::new(&self.executable);
        cmd.args(self.args(model, overlay.path())).current_dir(&self.cwd).kill_on_drop(true);
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let shown = self.executable.display();
        let mut child = loa_util::exec::retry_busy(|| cmd.spawn()).map_err(|e| format!("can't run {shown}: {e}"))?;
        let mut stdin = child.stdin.take().ok_or("no stdin pipe")?;
        let feed = async move {
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await
        };
        let run = async { tokio::join!(feed, child.wait_with_output()) };
        // On timeout the future is dropped, which kills the child.
        let (fed, output) = tokio::time::timeout(self.timeout, run).await.map_err(|_| format!("omp gave no answer within {:?}", self.timeout))?;
        let output = output.map_err(|e| format!("running {shown}: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("omp exited with {}: {}", output.status, loa_util::text::clip(stderr.trim(), 2000)));
        }
        fed.map_err(|e| format!("writing the prompt to omp: {e}"))?;
        String::from_utf8(output.stdout).map_err(|_| "omp printed invalid UTF-8".to_owned())
    }
}

#[async_trait]
impl Summarizer for OneShot {
    async fn summarize(&self, instructions: &str, input: &str) -> Result<String, String> {
        self.ask(&self.model, &format!("{instructions}{input}")).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_shot_runs_smol_without_session_or_memory_and_reports_failures() {
        let dir = tempfile::tempdir().unwrap();
        let (exe, d) = (dir.path().join("omp"), dir.path().display());
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{d}/args'\ncat > '{d}/stdin'\n\
             while [ $# -gt 0 ]; do if [ \"$1\" = --config ]; then cp \"$2\" '{d}/overlay'; fi; shift; done\n\
             if grep -q fail '{d}/stdin'; then echo 'no smol model configured' >&2; exit 2; fi\nprintf answer\n"
        );
        loa_util::exec::write_executable(&exe, script).unwrap();
        let omp = OneShot::new(&exe, dir.path());
        assert_eq!(omp.summarize("Summarize: ", "the input").await.unwrap(), "answer");
        let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
        let args: Vec<&str> = args.lines().collect();
        assert!(["-p", "--no-session", "--no-tools", "--no-title"].iter().all(|f| args.contains(f)), "{args:?}");
        assert_eq!(args[args.iter().position(|a| *a == "--model").unwrap() + 1], "@smol");
        assert_eq!(std::fs::read_to_string(dir.path().join("overlay")).unwrap(), MEMORY_OFF);
        assert_eq!(std::fs::read_to_string(dir.path().join("stdin")).unwrap(), "Summarize: the input");
        assert_eq!(omp.pick(Some(" a/b ")), "a/b");
        assert!(omp.ask("@smol", "fail").await.unwrap_err().contains("no smol model configured"));
        let missing = OneShot::new(dir.path().join("absent"), dir.path());
        assert!(missing.ask("@smol", "hi").await.unwrap_err().contains("can't run"));
    }
}
