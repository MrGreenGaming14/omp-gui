//! SFTP operations for `remote_file` (#99) over `russh-sftp`'s raw session,
//! which gives access to `posix-rename@openssh.com` for atomic replaces.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use russh::client;
use russh_sftp::client::RawSftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, OpenFlags, Packet, StatusCode};
use serde::Serialize;

use crate::connection::ClientHandler;
use crate::error::{Error, Result};
use crate::limits::{file_name, parent};

const POSIX_RENAME: &str = "posix-rename@openssh.com";
/// Bytes per READ / WRITE request; every SFTP server accepts 32 KiB.
const CHUNK: usize = 32 * 1024;

/// How a write reached the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Written to a temp file in the same directory, then renamed over the
    /// target: readers see the old or the new content, never a mix.
    Atomic,
    /// The server refused the temp file or the rename; the target was
    /// truncated and rewritten.
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

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_name(path: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = parent(path);
    let sep = if dir == "/" { "" } else { "/" };
    format!("{dir}{sep}.{}.loa-{nanos:x}-{n}.tmp", file_name(path))
}

fn encode_strings(parts: &[&str]) -> Vec<u8> {
    let mut out = Vec::with_capacity(parts.iter().map(|p| p.len() + 4).sum());
    for p in parts {
        // SFTP strings are length prefixed with a u32; paths never reach 4 GiB.
        out.extend_from_slice(&u32::try_from(p.len()).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(p.as_bytes());
    }
    out
}

impl Sftp {
    pub(crate) async fn open(handle: &client::Handle<ClientHandler>, host: &str) -> Result<Self> {
        let ssh = |source| Error::Ssh {
            host: host.to_string(),
            source,
        };
        let channel = handle.channel_open_session().await.map_err(ssh)?;
        channel.request_subsystem(true, "sftp").await.map_err(ssh)?;
        let raw = RawSftpSession::new(channel.into_stream());
        let version = raw.init().await.map_err(|e| Error::Sftp {
            host: host.to_string(),
            message: e.to_string(),
        })?;
        let posix_rename = version
            .extensions
            .get(POSIX_RENAME)
            .is_some_and(|v| v == "1");
        Ok(Self {
            raw,
            posix_rename,
            host: host.to_string(),
        })
    }

    fn err(&self, what: &str, path: &str, e: SftpError) -> Error {
        let message = match &e {
            SftpError::Status(s) => format!("status: {what} {path}: {}", s.status_code),
            other => format!("{what} {path}: {other}"),
        };
        Error::Sftp {
            host: self.host.clone(),
            message,
        }
    }

    /// The server's canonical absolute path, symlinks resolved.
    pub(crate) async fn realpath(&self, path: &str) -> Result<String> {
        let name = self
            .raw
            .realpath(path)
            .await
            .map_err(|e| self.err("realpath", path, e))?;
        name.files
            .into_iter()
            .next()
            .map(|f| f.filename)
            .ok_or_else(|| Error::Sftp {
                host: self.host.clone(),
                message: format!("realpath {path}: empty reply"),
            })
    }

    /// `lstat`, with "no such file" as `None`.
    pub(crate) async fn lstat(&self, path: &str) -> Result<Option<FileAttributes>> {
        match self.raw.lstat(path).await {
            Ok(a) => Ok(Some(a.attrs)),
            Err(e) if is_status(&e, StatusCode::NoSuchFile) => Ok(None),
            Err(e) => Err(self.err("lstat", path, e)),
        }
    }

    /// `stat` follows the final link and its parents, with "no such file" as `None`.
    pub(crate) async fn stat(&self, path: &str) -> Result<Option<FileAttributes>> {
        match self.raw.stat(path).await {
            Ok(a) => Ok(Some(a.attrs)),
            Err(e) if is_status(&e, StatusCode::NoSuchFile) => Ok(None),
            Err(e) => Err(self.err("stat", path, e)),
        }
    }

    /// Reads a whole file, refusing files over `max` bytes.
    pub(crate) async fn read(&self, path: &str, max: u64) -> Result<Vec<u8>> {
        let handle = self
            .raw
            .open(path, OpenFlags::READ, FileAttributes::empty())
            .await
            .map_err(|e| self.err("open", path, e))?
            .handle;
        let result = self.read_handle(&handle, path, max).await;
        let _ = self.raw.close(handle).await;
        result
    }

    async fn read_handle(&self, handle: &str, path: &str, max: u64) -> Result<Vec<u8>> {
        let size = self
            .raw
            .fstat(handle)
            .await
            .map_err(|e| self.err("fstat", path, e))?
            .attrs
            .size;
        if let Some(size) = size
            && size > max
        {
            return Err(Error::TooLarge {
                path: path.to_string(),
                size,
                max,
            });
        }
        let mut out = Vec::with_capacity(usize::try_from(size.unwrap_or(0)).unwrap_or(0));
        loop {
            match self.raw.read(handle, out.len() as u64, CHUNK as u32).await {
                Ok(data) if data.data.is_empty() => break,
                Ok(data) => {
                    out.extend_from_slice(&data.data);
                    if out.len() as u64 > max {
                        return Err(Error::TooLarge {
                            path: path.to_string(),
                            size: out.len() as u64,
                            max,
                        });
                    }
                }
                Err(e) if is_status(&e, StatusCode::Eof) => break,
                Err(e) => return Err(self.err("read", path, e)),
            }
        }
        Ok(out)
    }

    /// Lists a directory, without `.` and `..`.
    pub(crate) async fn list(&self, path: &str) -> Result<Vec<(String, FileAttributes)>> {
        let handle = self
            .raw
            .opendir(path)
            .await
            .map_err(|e| self.err("opendir", path, e))?
            .handle;
        let mut out = Vec::new();
        let result = loop {
            match self.raw.readdir(handle.as_str()).await {
                Ok(name) => out.extend(
                    name.files
                        .into_iter()
                        .filter(|f| f.filename != "." && f.filename != "..")
                        .map(|f| (f.filename, f.attrs)),
                ),
                Err(e) if is_status(&e, StatusCode::Eof) => break Ok(()),
                Err(e) => break Err(self.err("readdir", path, e)),
            }
        };
        let _ = self.raw.close(handle).await;
        result.map(|()| {
            out.sort_by(|a, b| a.0.cmp(&b.0));
            out
        })
    }

    /// Replaces `path` with `data`, atomically when the server allows.
    /// Keeps the permission bits of an existing file.
    pub(crate) async fn write(&self, path: &str, data: &[u8]) -> Result<WriteMode> {
        let permissions = self
            .lstat(path)
            .await?
            .and_then(|a| a.permissions)
            .map(|p| p & 0o7777);
        let attrs = FileAttributes {
            permissions,
            ..FileAttributes::empty()
        };
        let tmp = temp_name(path);
        let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE;
        let opened = match self.raw.open(tmp.as_str(), flags, attrs.clone()).await {
            Ok(h) => Some(h),
            Err(SftpError::Status(_)) => None,
            Err(e) => return Err(self.err("open", &tmp, e)),
        };
        if let Some(h) = opened {
            let written = self.write_handle(&h.handle, &tmp, data).await;
            let closed = self.raw.close(h.handle).await;
            if let Err(e) = written {
                let _ = self.raw.remove(tmp.as_str()).await;
                return Err(e);
            }
            if let Err(e) = closed {
                let _ = self.raw.remove(tmp.as_str()).await;
                return Err(self.err("close", &tmp, e));
            }
            match self.rename_over(&tmp, path).await {
                Ok(()) => return Ok(WriteMode::Atomic),
                Err(e) if is_status(&e, StatusCode::Failure) || is_status(&e, StatusCode::OpUnsupported) => {}
                Err(e) => {
                    let _ = self.raw.remove(tmp.as_str()).await;
                    return Err(self.err("rename", path, e));
                }
            }
            let _ = self.raw.remove(tmp.as_str()).await;
        }
        let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
        let h = self
            .raw
            .open(path, flags, attrs)
            .await
            .map_err(|e| self.err("open", path, e))?;
        let written = self.write_handle(&h.handle, path, data).await;
        let closed = self
            .raw
            .close(h.handle)
            .await
            .map_err(|e| self.err("close", path, e));
        written?;
        closed?;
        Ok(WriteMode::InPlace)
    }

    async fn rename_over(&self, from: &str, to: &str) -> std::result::Result<(), SftpError> {
        if self.posix_rename {
            match self
                .raw
                .extended(POSIX_RENAME, encode_strings(&[from, to]))
                .await?
            {
                Packet::Status(s) if s.status_code == StatusCode::Ok => Ok(()),
                Packet::Status(s) => Err(SftpError::Status(s)),
                _ => Err(SftpError::UnexpectedPacket),
            }
        } else {
            // Plain SFTPv3 rename fails when the target exists on OpenSSH;
            // the caller then falls back to an in-place write.
            self.raw.rename(from, to).await.map(|_| ())
        }
    }

    async fn write_handle(&self, handle: &str, path: &str, data: &[u8]) -> Result<()> {
        let mut offset = 0u64;
        for chunk in data.chunks(CHUNK) {
            self.raw
                .write(handle, offset, chunk.to_vec())
                .await
                .map_err(|e| self.err("write", path, e))?;
            offset += chunk.len() as u64;
        }
        Ok(())
    }
}
