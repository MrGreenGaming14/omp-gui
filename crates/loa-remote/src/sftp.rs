//! SFTP for `remote_file` over `russh-sftp`'s raw session, for `posix-rename@openssh.com` replaces.

use std::sync::atomic::{AtomicU64, Ordering};

use russh::client;
use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::extensions::HardlinkExtension;
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};
use serde::Serialize;

use crate::connection::ClientHandler;
use crate::error::{Error, Result};
use crate::limits::split;

const POSIX_RENAME: &str = "posix-rename@openssh.com";
/// Bytes per READ / WRITE request; every SFTP server accepts 32 KiB.
const CHUNK: usize = 32 * 1024;

/// How a write reached the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Written to a temp file in the same directory, then renamed over the target.
    Atomic,
    /// The server refused the temp file or the rename; the target was rewritten.
    InPlace,
}

/// One SFTP channel on a connection.
pub(crate) struct Sftp {
    raw: RawSftpSession,
    posix_rename: bool,
    host: String,
}

fn is_status(e: &SftpError, code: StatusCode) -> bool {
    matches!(e, SftpError::Status(s) if s.status_code == code)
}

/// Whether the error is the server answering (the channel still works).
pub(crate) fn is_server_status(e: &Error) -> bool {
    matches!(e, Error::Sftp { message, .. } if message.starts_with("status:"))
}

fn temp_name(path: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let (dir, name) = split(path);
    let sep = if dir == "/" { "" } else { "/" };
    format!("{dir}{sep}.{name}.loa-{:x}-{}.tmp", loa_util::time::now_us(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

impl Sftp {
    pub(crate) async fn open(handle: &client::Handle<ClientHandler>, host: &str) -> Result<Self> {
        let ssh = |source| Error::Ssh { host: host.to_owned(), source };
        let channel = handle.channel_open_session().await.map_err(ssh)?;
        channel.request_subsystem(true, "sftp").await.map_err(ssh)?;
        let raw = RawSftpSession::new(channel.into_stream());
        let version = raw.init().await.map_err(|e| Error::Sftp { host: host.to_owned(), message: e.to_string() })?;
        let posix_rename = version.extensions.get(POSIX_RENAME).is_some_and(|v| v == "1");
        Ok(Self { raw, posix_rename, host: host.to_owned() })
    }

    fn err(&self, what: &str, path: &str, e: SftpError) -> Error {
        let message = match &e {
            SftpError::Status(s) => format!("status: {what} {path}: {}", s.status_code),
            other => format!("{what} {path}: {other}"),
        };
        Error::Sftp { host: self.host.clone(), message }
    }

    /// The server's canonical absolute path, symlinks resolved.
    pub(crate) async fn realpath(&self, path: &str) -> Result<String> {
        let name = self.raw.realpath(path).await.map_err(|e| self.err("realpath", path, e))?;
        let empty = || Error::Sftp { host: self.host.clone(), message: format!("realpath {path}: empty reply") };
        name.files.into_iter().next().map(|f| f.filename).ok_or_else(empty)
    }

    /// `stat` (follow links) or `lstat`, with "no such file" as `None`.
    pub(crate) async fn stat(&self, path: &str, follow: bool) -> Result<Option<FileAttributes>> {
        let attrs = if follow { self.raw.stat(path).await } else { self.raw.lstat(path).await };
        match attrs {
            Ok(a) => Ok(Some(a.attrs)),
            Err(e) if is_status(&e, StatusCode::NoSuchFile) => Ok(None),
            Err(e) => Err(self.err("stat", path, e)),
        }
    }

    /// Reads a whole file, refusing files over `max` bytes.
    pub(crate) async fn read(&self, path: &str, max: u64) -> Result<Vec<u8>> {
        let handle = self.raw.open(path, OpenFlags::READ, FileAttributes::empty()).await.map_err(|e| self.err("open", path, e))?.handle;
        let result = self.read_handle(&handle, path, max).await;
        let _ = self.raw.close(handle).await;
        result
    }

    async fn read_handle(&self, handle: &str, path: &str, max: u64) -> Result<Vec<u8>> {
        let too_large = |size| Error::TooLarge { path: path.to_owned(), size, max };
        let size = self.raw.fstat(handle).await.map_err(|e| self.err("fstat", path, e))?.attrs.size;
        if let Some(size) = size.filter(|&s| s > max) {
            return Err(too_large(size));
        }
        let mut out = Vec::with_capacity(usize::try_from(size.unwrap_or(0)).unwrap_or(0));
        loop {
            match self.raw.read(handle, out.len() as u64, CHUNK as u32).await {
                Ok(data) if data.data.is_empty() => break,
                Ok(data) => out.extend_from_slice(&data.data),
                Err(e) if is_status(&e, StatusCode::Eof) => break,
                Err(e) => return Err(self.err("read", path, e)),
            }
            if out.len() as u64 > max {
                return Err(too_large(out.len() as u64));
            }
        }
        Ok(out)
    }

    /// Lists a directory, sorted, without `.` and `..`.
    pub(crate) async fn list(&self, path: &str) -> Result<Vec<(String, FileAttributes)>> {
        let handle = self.raw.opendir(path).await.map_err(|e| self.err("opendir", path, e))?.handle;
        let mut out = Vec::new();
        let result = loop {
            match self.raw.readdir(handle.as_str()).await {
                Ok(name) => out.extend(name.files.into_iter().filter(|f| f.filename != "." && f.filename != "..").map(|f| (f.filename, f.attrs))),
                Err(e) if is_status(&e, StatusCode::Eof) => break Ok(()),
                Err(e) => break Err(self.err("readdir", path, e)),
            }
        };
        let _ = self.raw.close(handle).await;
        out.sort_by(|a, b| a.0.cmp(&b.0));
        result.map(|()| out)
    }

    /// Replaces `path` with `data`, atomically when the server allows, keeping permission bits.
    pub(crate) async fn write(&self, path: &str, data: &[u8]) -> Result<WriteMode> {
        let permissions = self.stat(path, false).await?.and_then(|a| a.permissions).map(|p| p & 0o7777);
        let attrs = FileAttributes { permissions, ..FileAttributes::empty() };
        let tmp = temp_name(path);
        match self.raw.open(tmp.as_str(), OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE, attrs.clone()).await {
            Ok(h) => {
                let written = self.write_handle(&h.handle, &tmp, data).await;
                let closed = self.raw.close(h.handle).await.map(drop).map_err(|e| self.err("close", &tmp, e));
                let renamed = match written.and(closed) {
                    Ok(()) => self.rename_over(&tmp, path).await,
                    Err(e) => Err(e),
                };
                match renamed {
                    Ok(true) => return Ok(WriteMode::Atomic),
                    Ok(false) => drop(self.raw.remove(tmp.as_str()).await),
                    Err(e) => {
                        let _ = self.raw.remove(tmp.as_str()).await;
                        return Err(e);
                    }
                }
            }
            Err(SftpError::Status(_)) => {}
            Err(e) => return Err(self.err("open", &tmp, e)),
        }
        let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
        let h = self.raw.open(path, flags, attrs).await.map_err(|e| self.err("open", path, e))?;
        let written = self.write_handle(&h.handle, path, data).await;
        let closed = self.raw.close(h.handle).await.map(drop).map_err(|e| self.err("close", path, e));
        written.and(closed).map(|()| WriteMode::InPlace)
    }

    /// `Ok(false)` when the server won't replace `to` (plain SFTPv3 rename onto an existing file).
    async fn rename_over(&self, from: &str, to: &str) -> Result<bool> {
        let result = if self.posix_rename {
            let names = HardlinkExtension { oldpath: from.to_owned(), newpath: to.to_owned() };
            let body = russh_sftp::ser::to_bytes(&names).map_err(|e| Error::Sftp { host: self.host.clone(), message: e.to_string() })?;
            match self.raw.extended(POSIX_RENAME, body.to_vec()).await {
                Ok(Packet::Status(s)) if s.status_code == StatusCode::Ok => Ok(()),
                Ok(Packet::Status(s)) => Err(SftpError::Status(s)),
                Ok(_) => Err(SftpError::UnexpectedPacket),
                Err(e) => Err(e),
            }
        } else {
            self.raw.rename(from, to).await.map(drop)
        };
        match result {
            Ok(()) => Ok(true),
            Err(e) if is_status(&e, StatusCode::Failure) || is_status(&e, StatusCode::OpUnsupported) => Ok(false),
            Err(e) => Err(self.err("rename", to, e)),
        }
    }

    async fn write_handle(&self, handle: &str, path: &str, data: &[u8]) -> Result<()> {
        for (i, chunk) in data.chunks(CHUNK).enumerate() {
            self.raw.write(handle, (i * CHUNK) as u64, chunk.to_vec()).await.map_err(|e| self.err("write", path, e))?;
        }
        Ok(())
    }
}
