//! Wire protocol between the host and the exec stub inside a sandbox.
//!
//! The stub first writes [`MAGIC`], which lets the host find the protocol in
//! a serial console that also carries boot messages. After it, both
//! directions carry frames: a little endian `u32` body length, then a
//! postcard encoded [`Request`] or [`Response`]. Paths in requests are
//! relative to the stub's root directory; absolute paths are taken as they are.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

/// Protocol version the stub announces in [`Response::Ready`].
pub const VERSION: u32 = 2;
/// Written by the stub before its first frame.
pub const MAGIC: &[u8; 12] = b"\0LOA-STUB/2\n";
/// Largest frame body either side accepts.
pub const MAX_FRAME: u32 = 64 << 20;

/// What to run: a program and its arguments, or a script for `/bin/sh -c`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    Argv(Vec<String>),
    Shell(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecRequest {
    pub command: Command,
    /// Working directory, relative to the stub root. `None` is the root.
    pub cwd: Option<String>,
    /// Added to the stub's own environment.
    pub env: Vec<(String, String)>,
    /// Written to the command's stdin, which is then closed; `None` is an empty stdin.
    pub stdin: Option<Vec<u8>>,
    /// Kills the command and its process group after this many milliseconds.
    pub timeout_ms: Option<u64>,
}

impl ExecRequest {
    pub fn new(command: Command) -> Self {
        Self { command, cwd: None, env: Vec::new(), stdin: None, timeout_ms: None }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Answered by `Stdout`/`Stderr` chunks, then `Exit`.
    Exec { id: u32, exec: ExecRequest },
    /// Reads at most `limit` bytes. Answered by `Data`.
    ReadFile { id: u32, path: String, limit: Option<u64> },
    /// Creates parent directories. Answered by `Done`.
    WriteFile { id: u32, path: String, data: Vec<u8> },
    /// Answered by `Entries`.
    ListDir { id: u32, path: String },
    /// Kills every running exec and ends the stub.
    Shutdown,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitStatus {
    pub code: Option<i32>,
    /// Signal that ended the command, when it didn't exit.
    pub signal: Option<i32>,
    /// The command ran past its timeout and was killed.
    pub timed_out: bool,
}

impl ExitStatus {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    /// First frame after [`MAGIC`].
    Ready { version: u32 },
    Stdout { id: u32, data: Vec<u8> },
    Stderr { id: u32, data: Vec<u8> },
    Exit { id: u32, status: ExitStatus },
    Data { id: u32, data: Vec<u8> },
    Done { id: u32 },
    Entries { id: u32, entries: Vec<DirEntry> },
    Error { id: u32, message: String },
}

impl Response {
    /// The request this answers. `None` for `Ready`.
    pub fn id(&self) -> Option<u32> {
        use Response::*;
        match self {
            Ready { .. } => None,
            Stdout { id, .. } | Stderr { id, .. } | Exit { id, .. } | Data { id, .. } | Done { id } | Entries { id, .. } | Error { id, .. } => Some(*id),
        }
    }
}

/// One frame: the body length, then the postcard body.
pub fn frame(message: &impl Serialize) -> Vec<u8> {
    let body = postcard::to_stdvec(message).expect("protocol messages always serialize");
    let mut out = u32::try_from(body.len()).unwrap_or(u32::MAX).to_le_bytes().to_vec();
    out.extend(body);
    out
}

/// Decodes one frame body.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> io::Result<T> {
    postcard::from_bytes(body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("sandbox protocol: {e}")))
}

/// Reads one frame body into `buf`. `Ok(false)` on a clean end of stream before the frame starts.
pub fn read_frame(r: &mut impl Read, buf: &mut Vec<u8>) -> io::Result<bool> {
    let mut header = [0; 4];
    match r.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(header);
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {len} bytes is over the limit")));
    }
    buf.resize(len as usize, 0);
    r.read_exact(buf)?;
    Ok(true)
}

/// Consumes bytes until just after [`MAGIC`], discarding boot messages before it.
/// `Ok(false)` if the stream ends first.
pub fn skip_to_magic(r: &mut impl Read) -> io::Result<bool> {
    let (mut matched, mut byte) = (0, [0; 1]);
    while matched < MAGIC.len() {
        match r.read(&mut byte) {
            Ok(0) => return Ok(false),
            // MAGIC starts with a NUL found nowhere else in it, so a mismatch
            // can only restart the match at this byte.
            Ok(_) if MAGIC[matched] == byte[0] => matched += 1,
            Ok(_) => matched = usize::from(byte[0] == MAGIC[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Writes [`MAGIC`] and the `Ready` frame: the stub's first output.
pub fn write_hello(w: &mut impl Write) -> io::Result<()> {
    w.write_all(MAGIC)?;
    w.write_all(&frame(&Response::Ready { version: VERSION }))?;
    w.flush()
}
