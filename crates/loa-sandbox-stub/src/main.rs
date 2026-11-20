//! Thin exec stub that runs inside a sandbox.
//!
//! Speaks [`loa_sandbox::proto`] on stdin and stdout: runs commands, reads,
//! writes, lists and stats files under its root directory. It holds no
//! secrets and opens no connections; the environment it was started with is
//! all its commands see.
//!
//! Usage: `loa-sandbox-stub [ROOT]`. `ROOT` defaults to the current directory.

use loa_sandbox::proto::{
    self, Command, DirEntry, ExecRequest, ExitStatus, FileKind, Metadata, Request, Response, STDERR_TAG, STDOUT_TAG,
};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{self as sys, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Output chunk size for command stdout and stderr.
const CHUNK: usize = 32 * 1024;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every critical section here is a single insert, remove or write, so a
    // poisoned lock still guards consistent data.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Frames to the host. One lock per frame keeps frames whole.
#[derive(Clone)]
struct Out(Arc<Mutex<io::Stdout>>);

impl Out {
    fn frame(&self, encode: impl FnOnce(&mut Vec<u8>)) {
        let mut buf = Vec::new();
        encode(&mut buf);
        let mut out = lock(&self.0);
        // If the host is gone there is nobody to tell; the read loop sees the
        // closed stdin and ends the stub.
        let _ = out.write_all(&buf).and_then(|()| out.flush());
    }

    fn send(&self, response: &Response) {
        self.frame(|buf| response.encode(buf));
    }

    fn error(&self, id: u32, error: impl std::fmt::Display) {
        self.send(&Response::Error { id, message: error.to_string() });
    }
}

/// Process groups of running commands, by request id.
type Running = Arc<Mutex<HashMap<u32, i32>>>;

#[cfg(unix)]
fn kill_group(pgid: i32) {
    // SAFETY: kill(2) takes plain integers and has no memory effects. A
    // negative pid names the process group the command leads.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

/// Windows has no process groups: kill the command's whole process tree.
#[cfg(not(unix))]
fn kill_group(pid: i32) {
    let _ = sys::Command::new("taskkill")
        .args(["/F", "/T", "/PID"])
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

struct Stub {
    root: PathBuf,
    out: Out,
    running: Running,
}

impl Stub {
    fn path(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    fn handle(&self, request: Request) -> bool {
        match request {
            Request::Exec { id, exec } => self.exec(id, exec),
            Request::Kill { id } => {
                if let Some(pgid) = lock(&self.running).get(&id) {
                    kill_group(*pgid);
                }
            }
            Request::ReadFile { id, path, limit } => match read_file(&self.path(&path), limit) {
                Ok(data) => self.out.send(&Response::Data { id, data }),
                Err(e) => self.out.error(id, format_args!("{path}: {e}")),
            },
            Request::WriteFile { id, path, data, mode } => match write_file(&self.path(&path), &data, mode) {
                Ok(()) => self.out.send(&Response::Done { id }),
                Err(e) => self.out.error(id, format_args!("{path}: {e}")),
            },
            Request::ListDir { id, path } => match list_dir(&self.path(&path)) {
                Ok(entries) => self.out.send(&Response::Entries { id, entries }),
                Err(e) => self.out.error(id, format_args!("{path}: {e}")),
            },
            Request::Stat { id, path } => match fs::symlink_metadata(self.path(&path)) {
                Ok(meta) => self.out.send(&Response::Stat { id, meta: Some(metadata(&meta)) }),
                Err(e) if e.kind() == io::ErrorKind::NotFound => self.out.send(&Response::Stat { id, meta: None }),
                Err(e) => self.out.error(id, format_args!("{path}: {e}")),
            },
            Request::Shutdown => return false,
        }
        true
    }

    fn exec(&self, id: u32, exec: ExecRequest) {
        let ExecRequest { command, cwd, env, stdin, timeout_ms } = exec;
        let mut cmd = match command {
            Command::Argv(argv) => {
                let Some((program, args)) = argv.split_first() else {
                    return self.out.error(id, "empty argv");
                };
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
        cmd.current_dir(cwd.map_or_else(|| self.root.clone(), |c| self.path(&c)))
            .envs(env)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return self.out.error(id, format_args!("spawn: {e}")),
        };
        // The child leads its own group, so its pid is the group id. Pids fit
        // i32 on every unix.
        let pgid = i32::try_from(child.id()).unwrap_or(i32::MAX);
        lock(&self.running).insert(id, pgid);

        if let (Some(data), Some(mut pipe)) = (stdin, child.stdin.take()) {
            // A command that exits without reading its stdin closes the pipe;
            // that is its business, not an error.
            thread::spawn(move || {
                let _ = pipe.write_all(&data);
            });
        }
        let pumps: Vec<_> = [(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), STDOUT_TAG)]
            .into_iter()
            .chain([(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>), STDERR_TAG)])
            .filter_map(|(pipe, tag)| pipe.map(|p| (p, tag)))
            .map(|(pipe, tag)| {
                let out = self.out.clone();
                thread::spawn(move || pump(pipe, &out, tag, id))
            })
            .collect();

        let out = self.out.clone();
        let running = Arc::clone(&self.running);
        thread::spawn(move || {
            let finished = Arc::new((Mutex::new(false), Condvar::new()));
            let timer = timeout_ms.map(|ms| {
                let finished = Arc::clone(&finished);
                thread::spawn(move || {
                    let deadline = Instant::now() + Duration::from_millis(ms);
                    let (done, wake) = &*finished;
                    let mut done = lock(done);
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
            {
                let (done, wake) = &*finished;
                *lock(done) = true;
                wake.notify_all();
            }
            let timed_out = timer.is_some_and(|t| t.join().unwrap_or(false));
            // Output that is still buffered goes out before the exit.
            for pump in pumps {
                let _ = pump.join();
            }
            lock(&running).remove(&id);
            match status {
                Ok(status) => out.send(&Response::Exit {
                    id,
                    status: ExitStatus { code: status.code(), signal: exit_signal(&status), timed_out },
                }),
                Err(e) => out.error(id, format_args!("wait: {e}")),
            }
        });
    }
}

#[cfg(unix)]
fn exit_signal(status: &sys::ExitStatus) -> Option<i32> {
    status.signal()
}

#[cfg(not(unix))]
fn exit_signal(_: &sys::ExitStatus) -> Option<i32> {
    None
}

/// Forwards one output pipe as frames until it closes.
fn pump(mut pipe: Box<dyn Read + Send>, out: &Out, tag: u8, id: u32) {
    let mut buf = vec![0; CHUNK];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => out.frame(|frame| Response::encode_output(frame, tag, id, &buf[..n])),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

fn read_file(path: &Path, limit: Option<u64>) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let mut data = Vec::new();
    match limit {
        Some(limit) => file.take(limit).read_to_end(&mut data)?,
        None => (&file).read_to_end(&mut data)?,
    };
    Ok(data)
}

fn write_file(path: &Path, data: &[u8], mode: Option<u32>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, data)?;
    if let Some(mode) = mode {
        set_mode(path, mode)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// Windows only has a read-only flag, which stands for a missing owner write bit.
#[cfg(not(unix))]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, permissions)
}

fn list_dir(path: &Path) -> io::Result<Vec<DirEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        entries.push(DirEntry { name: entry.file_name().to_string_lossy().into_owned(), meta: metadata(&meta) });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

fn metadata(meta: &fs::Metadata) -> Metadata {
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        FileKind::Symlink
    } else if ft.is_dir() {
        FileKind::Dir
    } else if ft.is_file() {
        FileKind::File
    } else {
        FileKind::Other
    };
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    Metadata { kind, size: meta.len(), mode: file_mode(meta), mtime_ms }
}

#[cfg(unix)]
fn file_mode(meta: &fs::Metadata) -> u32 {
    meta.mode() & 0o7777
}

/// Windows has no mode bits: report what the read-only flag and the file
/// kind imply.
#[cfg(not(unix))]
fn file_mode(meta: &fs::Metadata) -> u32 {
    match (meta.is_dir(), meta.permissions().readonly()) {
        (true, _) => 0o755,
        (false, true) => 0o444,
        (false, false) => 0o644,
    }
}

fn main() {
    let root = match std::env::args_os().nth(1) {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    };
    let out = Out(Arc::new(Mutex::new(io::stdout())));
    if proto::write_hello(&mut *lock(&out.0)).is_err() {
        sys::exit(1);
    }
    let stub = Stub { root, out, running: Running::default() };
    let mut stdin = io::stdin().lock();
    let mut buf = Vec::new();
    while let Ok(true) = proto::read_frame(&mut stdin, &mut buf) {
        match Request::decode(&buf) {
            Ok(request) => {
                if !stub.handle(request) {
                    break;
                }
            }
            // A frame we can't read means the stream is out of step; there
            // is no safe way to continue.
            Err(_) => break,
        }
    }
    for pgid in lock(&stub.running).values() {
        kill_group(*pgid);
    }
    sys::exit(0);
}
