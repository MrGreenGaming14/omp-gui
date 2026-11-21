//! Thin exec stub that runs inside a sandbox.
//!
//! Speaks [`loa_sandbox::proto`] on stdin and stdout and runs commands in its
//! root directory. It holds no secrets and opens no connections; the
//! environment it was started with is all its commands see.
//!
//! Usage: `loa-sandbox-stub [ROOT]`. `ROOT` defaults to the current directory.
//!
//! On Windows a shell request, `/bin/sh -c SCRIPT sh`, runs through `cmd /C`
//! (cmd rather than PowerShell: it is always present and matches the simple
//! one-line commands agents send); one with positional arguments is a host
//! script for a POSIX shell and runs through the `sh` on `PATH` (Git for
//! Windows). Cancelling kills the whole process tree.

use loa_sandbox::proto::{self, ExecRequest, ExitStatus, Request, Response};
use loa_util::LockExt;
use std::collections::HashMap;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
#[cfg(windows)]
use process_wrap::std::{ChildWrapper, CommandWrap, JobObject};
use std::path::PathBuf;
use std::process::{self as sys, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Output chunk size for command stdout and stderr.
const CHUNK: usize = 32 * 1024;

/// Frames to the host. One lock per frame keeps frames whole.
#[derive(Clone)]
struct Out(Arc<Mutex<io::Stdout>>);

impl Out {
    fn send(&self, response: &Response) {
        let frame = proto::frame(response);
        let mut out = self.0.locked();
        // If the host is gone there is nobody to tell; the read loop sees the
        // closed stdin and ends the stub.
        let _ = out.write_all(&frame).and_then(|()| out.flush());
    }
}

/// Handles that kill a running command with everything it started, by request id.
type Running = Arc<Mutex<HashMap<u32, Killer>>>;

/// A started command: the handle to wait on, the way to kill its whole tree, and its pipes.
struct Spawned {
    proc: Proc,
    killer: Killer,
    stdin: Option<sys::ChildStdin>,
    stdout: Option<sys::ChildStdout>,
    stderr: Option<sys::ChildStderr>,
}

/// The process group the command leads.
#[cfg(unix)]
#[derive(Clone)]
struct Killer(i32);

#[cfg(unix)]
type Proc = sys::Child;

#[cfg(unix)]
impl Killer {
    fn kill(&self) {
        // SAFETY: kill(2) takes plain integers and has no memory effects. A
        // negative pid names the process group the command leads.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

#[cfg(unix)]
fn command(argv: &[String]) -> io::Result<sys::Command> {
    let (program, args) = argv.split_first().ok_or_else(|| io::Error::other("empty argv"))?;
    let mut cmd = sys::Command::new(program);
    cmd.args(args);
    Ok(cmd)
}

#[cfg(unix)]
fn spawn(mut cmd: sys::Command) -> io::Result<Spawned> {
    cmd.process_group(0);
    let mut child = cmd.spawn()?;
    // The child leads its own group, so its pid is the group id. Pids fit i32 on every unix.
    let killer = Killer(i32::try_from(child.id()).unwrap_or(i32::MAX));
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    Ok(Spawned { proc: child, killer, stdin, stdout, stderr })
}

#[cfg(unix)]
fn wait(proc: &mut Proc, _: &Killer) -> io::Result<sys::ExitStatus> {
    proc.wait()
}

#[cfg(unix)]
fn exit_signal(status: &sys::ExitStatus) -> Option<i32> {
    status.signal()
}

/// The command's Job Object: killing it ends the command and every process
/// it started, however deep.
#[cfg(windows)]
#[derive(Clone)]
struct Killer(Arc<Mutex<Box<dyn ChildWrapper>>>);

#[cfg(windows)]
type Proc = Killer;

#[cfg(windows)]
impl Killer {
    fn kill(&self) {
        let _ = self.0.locked().start_kill();
    }
}

/// `/bin/sh -c SCRIPT sh` becomes `cmd /S /C "SCRIPT"`. The script goes
/// through `raw_arg`: std's argument quoting is for CommandLineToArgvW, which
/// cmd does not use, and would mangle quotes inside the script. `/S` strips
/// just the outer pair added here. A shell request with positional arguments
/// needs a POSIX `sh`; any other argv runs as given.
#[cfg(windows)]
fn command(argv: &[String]) -> io::Result<sys::Command> {
    use std::os::windows::process::CommandExt;
    match argv {
        [sh, c, script, _] if sh == "/bin/sh" && c == "-c" => {
            let mut cmd = sys::Command::new("cmd");
            cmd.args(["/S", "/C"]).raw_arg(format!("\"{script}\""));
            Ok(cmd)
        }
        [sh, c, rest @ ..] if sh == "/bin/sh" && c == "-c" => {
            let sh = loa_util::exec::which("sh").ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "this request needs a POSIX sh on PATH (Git for Windows)"))?;
            let mut cmd = sys::Command::new(sh);
            cmd.arg("-c").args(rest);
            Ok(cmd)
        }
        [program, args @ ..] => {
            let mut cmd = sys::Command::new(program);
            cmd.args(args);
            Ok(cmd)
        }
        [] => Err(io::Error::other("empty argv")),
    }
}

/// Spawns `cmd` inside a Job Object (process-wrap creates it suspended,
/// assigns the process and resumes it, so nothing escapes the job).
#[cfg(windows)]
fn spawn(cmd: sys::Command) -> io::Result<Spawned> {
    let mut child = CommandWrap::from(cmd).wrap(JobObject).spawn()?;
    let (stdin, stdout, stderr) = (child.stdin().take(), child.stdout().take(), child.stderr().take());
    let killer = Killer(Arc::new(Mutex::new(child)));
    Ok(Spawned { proc: killer.clone(), killer, stdin, stdout, stderr })
}

/// Waits for the command to exit, then ends whatever it left running, so no
/// background process outlives its command or keeps its output pipes open.
/// Polls rather than blocks: killing needs the same child lock.
#[cfg(windows)]
fn wait(proc: &mut Proc, killer: &Killer) -> io::Result<sys::ExitStatus> {
    let status = loop {
        if let Some(status) = proc.0.locked().try_wait()? {
            break status;
        }
        thread::sleep(Duration::from_millis(5));
    };
    killer.kill();
    Ok(status)
}

#[cfg(not(unix))]
fn exit_signal(_: &sys::ExitStatus) -> Option<i32> {
    None
}

struct Stub {
    root: PathBuf,
    out: Out,
    running: Running,
}

impl Stub {
    fn handle(&self, request: Request) -> bool {
        let Request::Exec { id, exec } = request else { return false };
        if let Err(e) = self.exec(id, exec) {
            self.out.send(&Response::Error { id, message: format!("spawn: {e}") });
        }
        true
    }

    /// Starts the command with a pump per output pipe and a thread that
    /// waits for it (under a watchdog when it has a timeout), then reports its exit.
    fn exec(&self, id: u32, exec: ExecRequest) -> io::Result<()> {
        let ExecRequest { argv, cwd, env, stdin, timeout_ms } = exec;
        let mut cmd = command(&argv)?;
        cmd.current_dir(cwd.map_or_else(|| self.root.clone(), |cwd| self.root.join(cwd)))
            .envs(env).stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let Spawned { mut proc, killer, stdin: child_stdin, stdout, stderr } = spawn(cmd)?;
        self.running.locked().insert(id, killer.clone());
        if let (Some(data), Some(mut pipe)) = (stdin, child_stdin) {
            // A command may exit without reading its stdin; that is not an error.
            thread::spawn(move || pipe.write_all(&data));
        }
        let stdout = stdout.map(|pipe| pump(pipe, self.out.clone(), id, false));
        let stderr = stderr.map(|pipe| pump(pipe, self.out.clone(), id, true));
        let (out, running) = (self.out.clone(), Arc::clone(&self.running));
        thread::spawn(move || {
            let (status, timed_out) = watchdog(&mut proc, &killer, timeout_ms);
            // Output that is still buffered goes out before the exit.
            stdout.into_iter().chain(stderr).for_each(|pump| drop(pump.join()));
            running.locked().remove(&id);
            out.send(&match status {
                Ok(s) => Response::Exit { id, status: ExitStatus { code: s.code(), signal: exit_signal(&s), timed_out } },
                Err(e) => Response::Error { id, message: format!("wait: {e}") },
            });
        });
        Ok(())
    }
}

/// Forwards one output pipe as frames until it closes.
fn pump(mut pipe: impl Read + Send + 'static, out: Out, id: u32, stderr: bool) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = vec![0; CHUNK];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => return,
                Ok(n) if stderr => out.send(&Response::Stderr { id, data: buf[..n].to_vec() }),
                Ok(n) => out.send(&Response::Stdout { id, data: buf[..n].to_vec() }),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    })
}

/// Waits for the command, killing its tree once `timeout_ms` passes. Says whether it timed out.
fn watchdog(proc: &mut Proc, killer: &Killer, timeout_ms: Option<u64>) -> (io::Result<sys::ExitStatus>, bool) {
    let finished = Arc::new((Mutex::new(false), Condvar::new()));
    let timer = timeout_ms.map(|ms| {
        let (finished, killer) = (Arc::clone(&finished), killer.clone());
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(ms);
            let (done, wake) = &*finished;
            let mut done = done.locked();
            while !*done {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    killer.kill();
                    return true;
                }
                done = wake.wait_timeout(done, left).unwrap_or_else(PoisonError::into_inner).0;
            }
            false
        })
    });
    let status = wait(proc, killer);
    *finished.0.locked() = true;
    finished.1.notify_all();
    (status, timer.is_some_and(|t| t.join().unwrap_or(false)))
}

fn main() {
    let root = std::env::args_os().nth(1).map(PathBuf::from);
    let root = root.unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));
    let out = Out(Arc::new(Mutex::new(io::stdout())));
    if proto::write_hello(&mut *out.0.locked()).is_err() {
        sys::exit(1);
    }
    let stub = Stub { root, out, running: Running::default() };
    let (mut stdin, mut buf) = (io::stdin().lock(), Vec::new());
    // A frame we can't read means the stream is out of step; there is no safe way to continue.
    while let Ok(true) = proto::read_frame(&mut stdin, &mut buf) {
        if !proto::decode(&buf).is_ok_and(|request| stub.handle(request)) {
            break;
        }
    }
    for killer in stub.running.locked().values() {
        killer.kill();
    }
    sys::exit(0);
}
