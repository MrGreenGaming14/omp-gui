//! The process port: how this crate runs the harness CLIs (`claude`, `codex`).
//!
//! [`ProcessRunner`] runs a [`CommandSpec`] either to completion
//! ([`ProcessRunner::output`]) or as a live process the app shows in its own
//! terminal view ([`ProcessRunner::spawn`]). [`TokioProcessRunner`] is the real
//! implementation over `tokio::process`.

use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Error, Result};

/// A program invocation: program location, arguments, extra environment
/// variables and working directory. The child inherits the app's environment
/// plus `env`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Program to run; an absolute path or a name looked up on `PATH`.
    pub program: PathBuf,
    /// Arguments, without the program itself.
    pub args: Vec<String>,
    /// Environment variables set in addition to the inherited environment.
    pub env: Vec<(String, String)>,
    /// Working directory; `None` keeps the app's working directory.
    pub current_dir: Option<PathBuf>,
}

impl CommandSpec {
    /// A command running `program` with `args`, no extra environment and the
    /// app's working directory.
    pub fn new<I, S>(program: impl Into<PathBuf>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            env: Vec::new(),
            current_dir: None,
        }
    }

    /// The program as text, for error messages.
    pub fn program_name(&self) -> String {
        self.program.display().to_string()
    }
}

/// What a finished process printed and how it exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    /// Exit code; `None` when the process was ended by a signal.
    pub code: Option<i32>,
    /// Standard output, decoded as UTF-8 with invalid bytes replaced.
    pub stdout: String,
    /// Standard error, decoded as UTF-8 with invalid bytes replaced.
    pub stderr: String,
}

/// Readable end of a child's stdout or stderr pipe.
pub type ProcessReader = Box<dyn AsyncRead + Send + Unpin>;
/// Writable end of a child's stdin pipe.
pub type ProcessWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// Runs external programs.
#[async_trait]
pub trait ProcessRunner: Send + Sync {
    /// Runs `spec` to completion with stdin closed and returns its output.
    /// A program that cannot be started is an [`Error::Process`].
    async fn output(&self, spec: &CommandSpec) -> Result<ProcessOutput>;

    /// Starts `spec` with stdin, stdout and stderr piped so the app can show the
    /// process in its terminal view and forward what the user types. A program
    /// that cannot be started is an [`Error::Process`].
    async fn spawn(&self, spec: &CommandSpec) -> Result<Box<dyn RunningProcess>>;
}

/// A process started by [`ProcessRunner::spawn`].
///
/// The caller should read stdout and stderr while the process runs: a child that
/// fills a pipe nobody reads blocks.
#[async_trait]
pub trait RunningProcess: Send {
    /// Waits for the process to exit and returns its exit code (`None` when it
    /// was ended by a signal). Closes stdin first unless it was taken.
    async fn wait(&mut self) -> Result<Option<i32>>;

    /// Kills the process and waits for it to exit.
    async fn kill(&mut self) -> Result<()>;

    /// Takes the child's stdin; `None` after the first call.
    fn take_stdin(&mut self) -> Option<ProcessWriter>;

    /// Takes the child's stdout; `None` after the first call.
    fn take_stdout(&mut self) -> Option<ProcessReader>;

    /// Takes the child's stderr; `None` after the first call.
    fn take_stderr(&mut self) -> Option<ProcessReader>;
}

/// [`ProcessRunner`] over `tokio::process`. Spawned processes are killed when
/// their [`RunningProcess`] handle is dropped.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioProcessRunner;

impl TokioProcessRunner {
    fn command(spec: &CommandSpec) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(&spec.program);
        cmd.args(&spec.args);
        cmd.envs(spec.env.iter().map(|(k, v)| (k, v)));
        if let Some(dir) = &spec.current_dir {
            cmd.current_dir(dir);
        }
        cmd
    }
}

fn process_error(spec: &CommandSpec, err: &std::io::Error) -> Error {
    Error::Process {
        program: spec.program_name(),
        message: err.to_string(),
    }
}

#[async_trait]
impl ProcessRunner for TokioProcessRunner {
    async fn output(&self, spec: &CommandSpec) -> Result<ProcessOutput> {
        let mut cmd = Self::command(spec);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let out = cmd.output().await.map_err(|e| process_error(spec, &e))?;
        Ok(ProcessOutput {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    async fn spawn(&self, spec: &CommandSpec) -> Result<Box<dyn RunningProcess>> {
        let mut cmd = Self::command(spec);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = cmd.spawn().map_err(|e| process_error(spec, &e))?;
        Ok(Box::new(TokioRunningProcess {
            program: spec.program_name(),
            child,
        }))
    }
}

/// A child started by [`TokioProcessRunner::spawn`].
struct TokioRunningProcess {
    program: String,
    child: tokio::process::Child,
}

impl TokioRunningProcess {
    fn error(&self, err: &std::io::Error) -> Error {
        Error::Process {
            program: self.program.clone(),
            message: err.to_string(),
        }
    }
}

#[async_trait]
impl RunningProcess for TokioRunningProcess {
    async fn wait(&mut self) -> Result<Option<i32>> {
        match self.child.wait().await {
            Ok(status) => Ok(status.code()),
            Err(err) => Err(self.error(&err)),
        }
    }

    async fn kill(&mut self) -> Result<()> {
        match self.child.kill().await {
            Ok(()) => Ok(()),
            Err(err) => Err(self.error(&err)),
        }
    }

    fn take_stdin(&mut self) -> Option<ProcessWriter> {
        self.child
            .stdin
            .take()
            .map(|s| Box::new(s) as ProcessWriter)
    }

    fn take_stdout(&mut self) -> Option<ProcessReader> {
        self.child
            .stdout
            .take()
            .map(|s| Box::new(s) as ProcessReader)
    }

    fn take_stderr(&mut self) -> Option<ProcessReader> {
        self.child
            .stderr
            .take()
            .map(|s| Box::new(s) as ProcessReader)
    }
}

/// Test helpers shared by the harness tests.
#[cfg(test)]
pub(crate) mod testutil {
    use std::path::{Path, PathBuf};

    /// Writes an executable `#!/bin/sh` script named `name` into `dir` with
    /// `body` after the shebang line and returns its absolute path.
    pub(crate) fn fake_program(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        loa_util::exec::write_executable(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        path
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::testutil::fake_program;
    use super::*;

    #[tokio::test]
    async fn output_captures_streams_exit_code_and_env() {
        let dir = tempfile::tempdir().unwrap();
        let prog = fake_program(
            dir.path(),
            "tool",
            "printf 'out:%s:%s' \"$1\" \"$LOA_TEST_VAR\"\nprintf 'err' >&2\nexit 3",
        );
        let mut spec = CommandSpec::new(&prog, ["first arg"]);
        spec.env.push(("LOA_TEST_VAR".into(), "v1".into()));
        let out = TokioProcessRunner.output(&spec).await.unwrap();
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout, "out:first arg:v1");
        assert_eq!(out.stderr, "err");
    }

    #[tokio::test]
    async fn output_runs_in_current_dir() {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let prog = fake_program(dir.path(), "tool", "pwd");
        let mut spec = CommandSpec::new(&prog, Vec::<String>::new());
        spec.current_dir = Some(work.clone());
        let out = TokioProcessRunner.output(&spec).await.unwrap();
        let printed = std::fs::canonicalize(out.stdout.trim()).unwrap();
        assert_eq!(printed, std::fs::canonicalize(&work).unwrap());
    }

    #[tokio::test]
    async fn missing_program_is_a_process_error() {
        let dir = tempfile::tempdir().unwrap();
        let spec = CommandSpec::new(dir.path().join("absent"), ["x"]);
        let err = TokioProcessRunner.output(&spec).await.unwrap_err();
        assert!(matches!(err, Error::Process { .. }), "{err:?}");
        let err = TokioProcessRunner.spawn(&spec).await.err().unwrap();
        assert!(matches!(err, Error::Process { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn spawned_process_reads_stdin_and_writes_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let prog = fake_program(
            dir.path(),
            "tool",
            "printf 'Paste code: '\nread code\nprintf 'got %s' \"$code\"\nexit 5",
        );
        let spec = CommandSpec::new(&prog, Vec::<String>::new());
        let mut proc = TokioProcessRunner.spawn(&spec).await.unwrap();
        let mut stdin = proc.take_stdin().unwrap();
        stdin.write_all(b"abc123\n").await.unwrap();
        drop(stdin);
        let mut stdout = proc.take_stdout().unwrap();
        let mut text = String::new();
        stdout.read_to_string(&mut text).await.unwrap();
        assert_eq!(text, "Paste code: got abc123");
        assert_eq!(proc.wait().await.unwrap(), Some(5));
        assert!(proc.take_stdout().is_none());
    }

    #[tokio::test]
    async fn kill_ends_a_process_waiting_for_input() {
        let dir = tempfile::tempdir().unwrap();
        let prog = fake_program(dir.path(), "tool", "read line\nexit 0");
        let spec = CommandSpec::new(&prog, Vec::<String>::new());
        let mut proc = TokioProcessRunner.spawn(&spec).await.unwrap();
        let _stdin = proc.take_stdin().unwrap();
        proc.kill().await.unwrap();
        assert_eq!(proc.wait().await.unwrap(), None);
    }
}
