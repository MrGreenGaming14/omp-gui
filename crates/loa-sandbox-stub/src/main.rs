//! Thin exec stub that runs inside a sandbox.
//!
//! Speaks [`loa_sandbox::proto`] on stdin and stdout: runs commands, reads,
//! writes and lists files under its root directory. It holds no
//! secrets and opens no connections; the environment it was started with is
//! all its commands see.
//!
//! Usage: `loa-sandbox-stub [ROOT]`. `ROOT` defaults to the current directory.

use loa_sandbox::proto::{self, Command, DirEntry, ExecRequest, ExitStatus, FileKind, Request, Response};
use loa_util::LockExt;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{self as sys, Child, Stdio};
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

    fn reply<T>(&self, id: u32, path: &str, result: io::Result<T>, ok: impl FnOnce(T) -> Response) {
        self.send(&match result {
            Ok(value) => ok(value),
            Err(e) => Response::Error { id, message: format!("{path}: {e}") },
        });
    }
}

/// Process groups of running commands, by request id.
type Running = Arc<Mutex<HashMap<u32, i32>>>;

fn kill_group(pgid: i32) {
    // SAFETY: kill(2) takes plain integers and has no memory effects. A
    // negative pid names the process group the command leads.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

struct Stub {
    root: PathBuf,
    out: Out,
    running: Running,
}

impl Stub {
    fn handle(&self, request: Request) -> bool {
        let at = |path: &str| self.root.join(path);
        match request {
            Request::Exec { id, exec } => {
                if let Err(e) = self.exec(id, exec) {
                    self.out.send(&Response::Error { id, message: format!("spawn: {e}") });
                }
            }
            Request::ReadFile { id, path, limit } => {
                self.out.reply(id, &path, read_file(&at(&path), limit), |data| Response::Data { id, data });
            }
            Request::WriteFile { id, path, data } => {
                let path_buf = at(&path);
                let written = path_buf.parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(&path_buf, &data));
                self.out.reply(id, &path, written, |()| Response::Done { id });
            }
            Request::ListDir { id, path } => {
                self.out.reply(id, &path, list_dir(&at(&path)), |entries| Response::Entries { id, entries });
            }
            Request::Shutdown => return false,
        }
        true
    }

    /// Starts the command with a pump per output pipe and a thread that
    /// waits for it (under a watchdog when it has a timeout), then reports its exit.
    fn exec(&self, id: u32, exec: ExecRequest) -> io::Result<()> {
        let ExecRequest { command, cwd, env, stdin, timeout_ms } = exec;
        let mut child = spawn(command, cwd.map_or_else(|| self.root.clone(), |c| self.root.join(c)), env, stdin.is_some())?;
        // The child leads its own group, so its pid is the group id.
        let pgid = i32::try_from(child.id()).unwrap_or(i32::MAX);
        self.running.locked().insert(id, pgid);
        if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
            // A command may exit without reading its stdin; that is not an error.
            thread::spawn(move || pipe.write_all(&data));
        }
        let stdout = child.stdout.take().map(|pipe| pump(pipe, self.out.clone(), id, false));
        let stderr = child.stderr.take().map(|pipe| pump(pipe, self.out.clone(), id, true));
        let (out, running) = (self.out.clone(), Arc::clone(&self.running));
        thread::spawn(move || {
            let (status, timed_out) = watchdog(&mut child, pgid, timeout_ms);
            // Output that is still buffered goes out before the exit.
            stdout.into_iter().chain(stderr).for_each(|pump| drop(pump.join()));
            running.locked().remove(&id);
            out.send(&match status {
                Ok(s) => Response::Exit { id, status: ExitStatus { code: s.code(), signal: s.signal(), timed_out } },
                Err(e) => Response::Error { id, message: format!("wait: {e}") },
            });
        });
        Ok(())
    }
}

fn spawn(command: Command, cwd: PathBuf, env: Vec<(String, String)>, stdin: bool) -> io::Result<Child> {
    let mut cmd = match command {
        Command::Argv(argv) => {
            let (program, args) = argv.split_first().ok_or_else(|| io::Error::other("empty argv"))?;
            let mut cmd = sys::Command::new(program);
            cmd.args(args);
            cmd
        }
        Command::Shell(script) => {
            let mut cmd = sys::Command::new("/bin/sh");
            cmd.arg("-c").arg(script);
            cmd
        }
    };
    cmd.current_dir(cwd).envs(env).stdin(if stdin { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).spawn()
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

/// Waits for `child`, killing its group once `timeout_ms` passes. Says whether it timed out.
fn watchdog(child: &mut Child, pgid: i32, timeout_ms: Option<u64>) -> (io::Result<sys::ExitStatus>, bool) {
    let finished = Arc::new((Mutex::new(false), Condvar::new()));
    let timer = timeout_ms.map(|ms| {
        let finished = Arc::clone(&finished);
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(ms);
            let (done, wake) = &*finished;
            let mut done = done.locked();
            while !*done {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    kill_group(pgid);
                    return true;
                }
                done = wake.wait_timeout(done, left).unwrap_or_else(PoisonError::into_inner).0;
            }
            false
        })
    });
    let status = child.wait();
    *finished.0.locked() = true;
    finished.1.notify_all();
    (status, timer.is_some_and(|t| t.join().unwrap_or(false)))
}

fn read_file(path: &Path, limit: Option<u64>) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    fs::File::open(path)?.take(limit.unwrap_or(u64::MAX)).read_to_end(&mut data)?;
    Ok(data)
}

/// Entries sorted by name; symlinks are not followed.
fn list_dir(path: &Path) -> io::Result<Vec<DirEntry>> {
    let mut entries = fs::read_dir(path)?
        .map(|entry| {
            let entry = entry?;
            let t = entry.file_type()?;
            let kind = if t.is_symlink() { FileKind::Symlink } else if t.is_dir() { FileKind::Dir } else if t.is_file() { FileKind::File } else { FileKind::Other };
            Ok(DirEntry { name: entry.file_name().to_string_lossy().into_owned(), kind, size: entry.metadata()?.len() })
        })
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
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
    for pgid in stub.running.locked().values() {
        kill_group(*pgid);
    }
    sys::exit(0);
}
