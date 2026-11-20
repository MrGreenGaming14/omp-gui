//! Wire protocol between the host and the exec stub inside a sandbox.
//!
//! Std only, so the stub links nothing else. The stream starts with
//! [`MAGIC`] from the stub, which lets the host find the protocol in a serial
//! console that also carries boot messages. After it, both directions carry
//! frames: a little endian `u32` body length, then the body. A body is a tag
//! byte followed by the message fields in order: integers little endian,
//! byte strings and strings as a `u32` length then the bytes, options as a
//! `0`/`1` byte then the value, lists as a `u32` count then the items.
//!
//! Paths in requests are relative to the stub's root directory; absolute
//! paths are taken as they are.

use std::fmt;
use std::io::{self, Read, Write};

/// Protocol version the stub announces in [`Response::Ready`].
pub const VERSION: u32 = 1;

/// Written by the stub before its first frame.
pub const MAGIC: &[u8; 12] = b"\0LOA-STUB/1\n";

/// Largest frame body either side accepts.
pub const MAX_FRAME: u32 = 64 << 20;

/// What to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Program and arguments, run directly.
    Argv(Vec<String>),
    /// A script for `/bin/sh -c`, or `cmd /C` on Windows.
    Shell(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecRequest {
    pub command: Command,
    /// Working directory, relative to the stub root. `None` is the root.
    pub cwd: Option<String>,
    /// Added to the stub's own environment.
    pub env: Vec<(String, String)>,
    /// Written to the command's stdin, which is then closed. `None` gives it
    /// an empty stdin.
    pub stdin: Option<Vec<u8>>,
    /// Kills the command and its process group after this many milliseconds.
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Runs a command. Answered by `Stdout`/`Stderr` chunks, then `Exit`.
    Exec { id: u32, exec: ExecRequest },
    /// Kills the process group of the running exec `id`. Its `Exit` follows.
    Kill { id: u32 },
    /// Reads a file, at most `limit` bytes. Answered by `Data`.
    ReadFile { id: u32, path: String, limit: Option<u64> },
    /// Writes a file, creating parent directories. Answered by `Done`.
    WriteFile { id: u32, path: String, data: Vec<u8>, mode: Option<u32> },
    /// Lists a directory. Answered by `Entries`.
    ListDir { id: u32, path: String },
    /// Answered by `Stat`, without following a final symlink.
    Stat { id: u32, path: String },
    /// Kills every running exec and ends the stub.
    Shutdown,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub kind: FileKind,
    pub size: u64,
    /// Unix permission bits.
    pub mode: u32,
    /// Unix milliseconds.
    pub mtime_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub meta: Metadata,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    /// First frame after [`MAGIC`].
    Ready { version: u32 },
    Stdout { id: u32, data: Vec<u8> },
    Stderr { id: u32, data: Vec<u8> },
    Exit { id: u32, status: ExitStatus },
    Data { id: u32, data: Vec<u8> },
    Done { id: u32 },
    Entries { id: u32, entries: Vec<DirEntry> },
    /// `None` when the path doesn't exist.
    Stat { id: u32, meta: Option<Metadata> },
    Error { id: u32, message: String },
}

impl Response {
    /// The request this answers. `None` for `Ready`.
    pub fn id(&self) -> Option<u32> {
        match self {
            Response::Ready { .. } => None,
            Response::Stdout { id, .. }
            | Response::Stderr { id, .. }
            | Response::Exit { id, .. }
            | Response::Data { id, .. }
            | Response::Done { id }
            | Response::Entries { id, .. }
            | Response::Stat { id, .. }
            | Response::Error { id, .. } => Some(*id),
        }
    }
}

/// A frame body that doesn't decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtoError(pub String);

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sandbox protocol: {}", self.0)
    }
}

impl std::error::Error for ProtoError {}

impl From<ProtoError> for io::Error {
    fn from(e: ProtoError) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }
}

/// Appends frames to a buffer: reserve the length, write the body, patch it.
struct Enc<'a> {
    out: &'a mut Vec<u8>,
    start: usize,
}

impl<'a> Enc<'a> {
    fn frame(out: &'a mut Vec<u8>, tag: u8) -> Self {
        let start = out.len();
        out.extend_from_slice(&[0; 4]);
        out.push(tag);
        Self { out, start }
    }

    fn u8(&mut self, v: u8) -> &mut Self {
        self.out.push(v);
        self
    }

    fn u32(&mut self, v: u32) -> &mut Self {
        self.out.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn u64(&mut self, v: u64) -> &mut Self {
        self.out.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn i32(&mut self, v: i32) -> &mut Self {
        self.out.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn i64(&mut self, v: i64) -> &mut Self {
        self.out.extend_from_slice(&v.to_le_bytes());
        self
    }

    fn len(&mut self, n: usize) -> &mut Self {
        // Frames over `MAX_FRAME` are refused by the reader, so a length that
        // doesn't fit u32 never makes a valid frame either way.
        self.u32(u32::try_from(n).unwrap_or(u32::MAX))
    }

    fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.len(v.len());
        self.out.extend_from_slice(v);
        self
    }

    fn str(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes())
    }

    fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(u8::from(v))
    }

    fn opt<T>(&mut self, v: Option<T>, f: impl FnOnce(&mut Self, T)) -> &mut Self {
        match v {
            Some(v) => {
                self.u8(1);
                f(self, v);
            }
            None => {
                self.u8(0);
            }
        }
        self
    }

    fn meta(&mut self, m: &Metadata) -> &mut Self {
        let kind = match m.kind {
            FileKind::File => 0,
            FileKind::Dir => 1,
            FileKind::Symlink => 2,
            FileKind::Other => 3,
        };
        self.u8(kind).u64(m.size).u32(m.mode).i64(m.mtime_ms)
    }
}

impl Drop for Enc<'_> {
    fn drop(&mut self) {
        let body = self.out.len() - self.start - 4;
        let len = u32::try_from(body).unwrap_or(u32::MAX).to_le_bytes();
        self.out[self.start..self.start + 4].copy_from_slice(&len);
    }
}

/// Reads fields from a frame body.
struct Dec<'a> {
    buf: &'a [u8],
}

impl<'a> Dec<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtoError> {
        if self.buf.len() < n {
            return Err(ProtoError("truncated frame".into()));
        }
        let (head, rest) = self.buf.split_at(n);
        self.buf = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProtoError> {
        let mut out = [0; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, ProtoError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ProtoError> {
        self.array().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, ProtoError> {
        self.array().map(u64::from_le_bytes)
    }

    fn i32(&mut self) -> Result<i32, ProtoError> {
        self.array().map(i32::from_le_bytes)
    }

    fn i64(&mut self) -> Result<i64, ProtoError> {
        self.array().map(i64::from_le_bytes)
    }

    fn bool(&mut self) -> Result<bool, ProtoError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            b => Err(ProtoError(format!("bad bool {b}"))),
        }
    }

    fn bytes(&mut self) -> Result<Vec<u8>, ProtoError> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }

    fn str(&mut self) -> Result<String, ProtoError> {
        String::from_utf8(self.bytes()?).map_err(|_| ProtoError("string is not UTF-8".into()))
    }

    fn opt<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T, ProtoError>) -> Result<Option<T>, ProtoError> {
        if self.bool()? { f(self).map(Some) } else { Ok(None) }
    }

    fn count(&mut self) -> Result<usize, ProtoError> {
        let n = self.u32()? as usize;
        // Every item takes at least one byte, so a larger count is corrupt.
        if n > self.buf.len() {
            return Err(ProtoError("list count past the frame end".into()));
        }
        Ok(n)
    }

    fn meta(&mut self) -> Result<Metadata, ProtoError> {
        let kind = match self.u8()? {
            0 => FileKind::File,
            1 => FileKind::Dir,
            2 => FileKind::Symlink,
            3 => FileKind::Other,
            k => return Err(ProtoError(format!("bad file kind {k}"))),
        };
        Ok(Metadata { kind, size: self.u64()?, mode: self.u32()?, mtime_ms: self.i64()? })
    }

    fn end(self) -> Result<(), ProtoError> {
        if self.buf.is_empty() { Ok(()) } else { Err(ProtoError("trailing bytes in frame".into())) }
    }
}

mod tag {
    pub const EXEC: u8 = 1;
    pub const KILL: u8 = 2;
    pub const READ_FILE: u8 = 3;
    pub const WRITE_FILE: u8 = 4;
    pub const LIST_DIR: u8 = 5;
    pub const STAT: u8 = 6;
    pub const SHUTDOWN: u8 = 7;

    pub const READY: u8 = 128;
    pub const STDOUT: u8 = 129;
    pub const STDERR: u8 = 130;
    pub const EXIT: u8 = 131;
    pub const DATA: u8 = 132;
    pub const DONE: u8 = 133;
    pub const ENTRIES: u8 = 134;
    pub const STAT_REPLY: u8 = 135;
    pub const ERROR: u8 = 136;
}

impl Request {
    /// Appends this request as one frame (length and body) to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Request::Exec { id, exec } => {
                let mut e = Enc::frame(out, tag::EXEC);
                e.u32(*id);
                match &exec.command {
                    Command::Argv(argv) => {
                        e.u8(0).len(argv.len());
                        for arg in argv {
                            e.str(arg);
                        }
                    }
                    Command::Shell(script) => {
                        e.u8(1).str(script);
                    }
                }
                e.opt(exec.cwd.as_deref(), |e, v| {
                    e.str(v);
                });
                e.len(exec.env.len());
                for (k, v) in &exec.env {
                    e.str(k).str(v);
                }
                e.opt(exec.stdin.as_deref(), |e, v| {
                    e.bytes(v);
                });
                e.opt(exec.timeout_ms, |e, v| {
                    e.u64(v);
                });
            }
            Request::Kill { id } => {
                Enc::frame(out, tag::KILL).u32(*id);
            }
            Request::ReadFile { id, path, limit } => {
                Enc::frame(out, tag::READ_FILE).u32(*id).str(path).opt(*limit, |e, v| {
                    e.u64(v);
                });
            }
            Request::WriteFile { id, path, data, mode } => {
                Enc::frame(out, tag::WRITE_FILE).u32(*id).str(path).bytes(data).opt(*mode, |e, v| {
                    e.u32(v);
                });
            }
            Request::ListDir { id, path } => {
                Enc::frame(out, tag::LIST_DIR).u32(*id).str(path);
            }
            Request::Stat { id, path } => {
                Enc::frame(out, tag::STAT).u32(*id).str(path);
            }
            Request::Shutdown => {
                Enc::frame(out, tag::SHUTDOWN);
            }
        }
    }

    /// Decodes one frame body.
    pub fn decode(body: &[u8]) -> Result<Self, ProtoError> {
        let mut d = Dec { buf: body };
        let request = match d.u8()? {
            tag::EXEC => {
                let id = d.u32()?;
                let command = match d.u8()? {
                    0 => {
                        let n = d.count()?;
                        let mut argv = Vec::with_capacity(n);
                        for _ in 0..n {
                            argv.push(d.str()?);
                        }
                        Command::Argv(argv)
                    }
                    1 => Command::Shell(d.str()?),
                    k => return Err(ProtoError(format!("bad command kind {k}"))),
                };
                let cwd = d.opt(Dec::str)?;
                let n = d.count()?;
                let mut env = Vec::with_capacity(n);
                for _ in 0..n {
                    env.push((d.str()?, d.str()?));
                }
                let stdin = d.opt(Dec::bytes)?;
                let timeout_ms = d.opt(Dec::u64)?;
                Request::Exec { id, exec: ExecRequest { command, cwd, env, stdin, timeout_ms } }
            }
            tag::KILL => Request::Kill { id: d.u32()? },
            tag::READ_FILE => Request::ReadFile { id: d.u32()?, path: d.str()?, limit: d.opt(Dec::u64)? },
            tag::WRITE_FILE => {
                Request::WriteFile { id: d.u32()?, path: d.str()?, data: d.bytes()?, mode: d.opt(Dec::u32)? }
            }
            tag::LIST_DIR => Request::ListDir { id: d.u32()?, path: d.str()? },
            tag::STAT => Request::Stat { id: d.u32()?, path: d.str()? },
            tag::SHUTDOWN => Request::Shutdown,
            t => return Err(ProtoError(format!("unknown request tag {t}"))),
        };
        d.end()?;
        Ok(request)
    }
}

impl Response {
    /// Appends this response as one frame (length and body) to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Response::Ready { version } => {
                Enc::frame(out, tag::READY).u32(*version);
            }
            Response::Stdout { id, data } => Self::encode_output(out, tag::STDOUT, *id, data),
            Response::Stderr { id, data } => Self::encode_output(out, tag::STDERR, *id, data),
            Response::Exit { id, status } => {
                Enc::frame(out, tag::EXIT)
                    .u32(*id)
                    .opt(status.code, |e, v| {
                        e.i32(v);
                    })
                    .opt(status.signal, |e, v| {
                        e.i32(v);
                    })
                    .bool(status.timed_out);
            }
            Response::Data { id, data } => {
                Enc::frame(out, tag::DATA).u32(*id).bytes(data);
            }
            Response::Done { id } => {
                Enc::frame(out, tag::DONE).u32(*id);
            }
            Response::Entries { id, entries } => {
                let mut e = Enc::frame(out, tag::ENTRIES);
                e.u32(*id).len(entries.len());
                for entry in entries {
                    e.str(&entry.name).meta(&entry.meta);
                }
            }
            Response::Stat { id, meta } => {
                Enc::frame(out, tag::STAT_REPLY).u32(*id).opt(meta.as_ref(), |e, m| {
                    e.meta(m);
                });
            }
            Response::Error { id, message } => {
                Enc::frame(out, tag::ERROR).u32(*id).str(message);
            }
        }
    }

    /// Appends a `Stdout` or `Stderr` frame straight from a read buffer,
    /// without building a [`Response`] first.
    pub fn encode_output(out: &mut Vec<u8>, stream_tag: u8, id: u32, data: &[u8]) {
        Enc::frame(out, stream_tag).u32(id).bytes(data);
    }

    /// Decodes one frame body.
    pub fn decode(body: &[u8]) -> Result<Self, ProtoError> {
        let mut d = Dec { buf: body };
        let response = match d.u8()? {
            tag::READY => Response::Ready { version: d.u32()? },
            tag::STDOUT => Response::Stdout { id: d.u32()?, data: d.bytes()? },
            tag::STDERR => Response::Stderr { id: d.u32()?, data: d.bytes()? },
            tag::EXIT => Response::Exit {
                id: d.u32()?,
                status: ExitStatus { code: d.opt(Dec::i32)?, signal: d.opt(Dec::i32)?, timed_out: d.bool()? },
            },
            tag::DATA => Response::Data { id: d.u32()?, data: d.bytes()? },
            tag::DONE => Response::Done { id: d.u32()? },
            tag::ENTRIES => {
                let id = d.u32()?;
                let n = d.count()?;
                let mut entries = Vec::with_capacity(n);
                for _ in 0..n {
                    entries.push(DirEntry { name: d.str()?, meta: d.meta()? });
                }
                Response::Entries { id, entries }
            }
            tag::STAT_REPLY => Response::Stat { id: d.u32()?, meta: d.opt(Dec::meta)? },
            tag::ERROR => Response::Error { id: d.u32()?, message: d.str()? },
            t => return Err(ProtoError(format!("unknown response tag {t}"))),
        };
        d.end()?;
        Ok(response)
    }
}

/// Tag of a `Stdout` frame, for [`Response::encode_output`].
pub const STDOUT_TAG: u8 = tag::STDOUT;
/// Tag of a `Stderr` frame, for [`Response::encode_output`].
pub const STDERR_TAG: u8 = tag::STDERR;

/// Checks a frame length read from the stream.
pub fn frame_len(header: [u8; 4]) -> Result<usize, ProtoError> {
    let len = u32::from_le_bytes(header);
    if len > MAX_FRAME {
        return Err(ProtoError(format!("frame of {len} bytes is over the {MAX_FRAME} byte limit")));
    }
    Ok(len as usize)
}

/// Reads one frame body into `buf`. `Ok(false)` on a clean end of stream
/// before the frame starts.
pub fn read_frame(r: &mut impl Read, buf: &mut Vec<u8>) -> io::Result<bool> {
    let mut header = [0; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut header[got..]) {
            Ok(0) if got == 0 => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = frame_len(header)?;
    buf.clear();
    buf.resize(len, 0);
    r.read_exact(buf)?;
    Ok(true)
}

/// Consumes bytes until just after [`MAGIC`]. Anything before it (boot
/// messages on a serial console) is discarded. `Ok(false)` if the stream
/// ends first.
pub fn skip_to_magic(r: &mut impl Read) -> io::Result<bool> {
    let mut matched = 0;
    let mut byte = [0; 1];
    while matched < MAGIC.len() {
        match r.read(&mut byte) {
            Ok(0) => return Ok(false),
            Ok(_) => matched = advance_magic(matched, byte[0]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// One step of matching [`MAGIC`] in a byte stream: how many leading bytes
/// of it are matched after `byte`, given `matched` before.
pub fn advance_magic(matched: usize, byte: u8) -> usize {
    if MAGIC[matched] == byte {
        return matched + 1;
    }
    // MAGIC starts with a NUL that appears nowhere else in it, so a mismatch
    // can only restart the match at this byte.
    usize::from(byte == MAGIC[0])
}

/// Writes [`MAGIC`] and the `Ready` frame: the stub's first output.
pub fn write_hello(w: &mut impl Write) -> io::Result<()> {
    let mut out = MAGIC.to_vec();
    Response::Ready { version: VERSION }.encode(&mut out);
    w.write_all(&out)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_request(r: Request) {
        let mut out = Vec::new();
        r.encode(&mut out);
        let len = frame_len(out[..4].try_into().unwrap()).unwrap();
        assert_eq!(len, out.len() - 4);
        assert_eq!(Request::decode(&out[4..]).unwrap(), r);
    }

    fn round_response(r: Response) {
        let mut out = Vec::new();
        r.encode(&mut out);
        assert_eq!(Response::decode(&out[4..]).unwrap(), r);
    }

    #[test]
    fn every_message_survives_the_wire() {
        round_request(Request::Exec {
            id: 7,
            exec: ExecRequest {
                command: Command::Argv(vec!["ls".into(), "-la".into(), "ü".into()]),
                cwd: Some("src".into()),
                env: vec![("A".into(), "b=c".into())],
                stdin: Some(vec![0, 1, 2, 255]),
                timeout_ms: Some(1500),
            },
        });
        round_request(Request::Exec {
            id: 8,
            exec: ExecRequest { command: Command::Shell("echo hi".into()), cwd: None, env: vec![], stdin: None, timeout_ms: None },
        });
        round_request(Request::Kill { id: 7 });
        round_request(Request::ReadFile { id: 1, path: "a/b".into(), limit: Some(10) });
        round_request(Request::WriteFile { id: 2, path: "x".into(), data: b"\0bin".to_vec(), mode: Some(0o755) });
        round_request(Request::ListDir { id: 3, path: ".".into() });
        round_request(Request::Stat { id: 4, path: "nope".into() });
        round_request(Request::Shutdown);

        let meta = Metadata { kind: FileKind::Symlink, size: 3, mode: 0o777, mtime_ms: -5 };
        round_response(Response::Ready { version: VERSION });
        round_response(Response::Stdout { id: 1, data: b"out".to_vec() });
        round_response(Response::Stderr { id: 1, data: vec![] });
        round_response(Response::Exit { id: 1, status: ExitStatus { code: None, signal: Some(9), timed_out: true } });
        round_response(Response::Data { id: 2, data: vec![9; 1000] });
        round_response(Response::Done { id: 3 });
        round_response(Response::Entries { id: 4, entries: vec![DirEntry { name: "f".into(), meta: meta.clone() }] });
        round_response(Response::Stat { id: 5, meta: Some(meta) });
        round_response(Response::Stat { id: 6, meta: None });
        round_response(Response::Error { id: 7, message: "denied".into() });
    }

    #[test]
    fn corrupt_frames_are_rejected_not_misread() {
        let mut out = Vec::new();
        Request::ReadFile { id: 1, path: "abc".into(), limit: None }.encode(&mut out);
        let body = &out[4..];
        assert!(Request::decode(&body[..body.len() - 1]).is_err());
        let mut longer = body.to_vec();
        longer.push(0);
        assert!(Request::decode(&longer).is_err());
        assert!(Request::decode(&[99]).is_err());
        // A list count far past the frame end must not allocate for it.
        assert!(Response::decode(&[tag::ENTRIES, 1, 0, 0, 0, 255, 255, 255, 255]).is_err());
        assert!(frame_len((MAX_FRAME + 1).to_le_bytes()).is_err());
    }

    #[test]
    fn host_finds_the_protocol_after_boot_noise() {
        let mut stream = b"Booting kernel...\n\0\0LOA-STU\0garbage\n".to_vec();
        write_hello(&mut stream).unwrap();
        let mut r = &stream[..];
        assert!(skip_to_magic(&mut r).unwrap());
        let mut buf = Vec::new();
        assert!(read_frame(&mut r, &mut buf).unwrap());
        assert_eq!(Response::decode(&buf).unwrap(), Response::Ready { version: VERSION });
        assert!(!skip_to_magic(&mut &b"no magic here"[..]).unwrap());
    }
}
