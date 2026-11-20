//! `omp/folder/1`: folders this node shares through folder grants. A peer
//! holding a grant on a folder (or a folder above it) lists it and reads its
//! files; a write grant also writes them. Every path stays inside the
//! granted folder: `..`, absolute paths and links that lead outside are
//! refused.
//!
//! One request per stream: the requester sends its request, answers the
//! grant challenge of `omp/session/1`, and gets a reply saying whether the
//! request may go ahead. A write then sends the file's bytes; a read gets
//! them back in chunks.

use crate::share::{challenge_grants, present_grants};
use crate::{Inner, Node, NodeError, Result, now_ms};
use async_trait::async_trait;
use loa_grants::{Action, Request, Revocations, VerifiedGrant};
use loa_net::NodeId;
use loa_net::conn::{Connection, RecvHalf, Stream};
use loa_net::control::{recv_message, send_message};
use loa_net::router::ProtocolHandler;
use std::path::{Component, Path, PathBuf};
use std::sync::Weak;

/// Protocol name of shared folders.
pub const PROTOCOL: &str = "omp/folder";
/// Largest file read or written over the protocol.
pub const MAX_FILE: u64 = 16 * 1024 * 1024;
/// File bytes per frame, well under the 1 MiB frame limit.
const CHUNK: usize = 512 * 1024;

/// One entry of a shared folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEntry {
    pub name: String,
    pub dir: bool,
    /// Bytes, for files.
    pub size: u64,
}

/// A folder of a peer this node's grants reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedFolder {
    pub peer: NodeId,
    /// The folder's absolute path on the peer.
    pub path: String,
    pub can_write: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    List,
    Read,
    Write,
}

impl Op {
    const fn as_str(self) -> &'static str {
        match self {
            Op::List => "list",
            Op::Read => "read",
            Op::Write => "write",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Op::List, Op::Read, Op::Write].into_iter().find(|op| op.as_str() == text)
    }

    const fn action(self) -> Action {
        match self {
            Op::List | Op::Read => Action::Read,
            Op::Write => Action::Write,
        }
    }
}

/// The opening request, and each later chunk of a write (`data` only).
#[derive(Clone, PartialEq, prost::Message)]
struct FolderRequest {
    #[prost(string, tag = "1")]
    op: String,
    /// The granted folder (or one below it), absolute on the owner.
    #[prost(string, tag = "2")]
    folder: String,
    /// Relative to `folder`; empty for the folder itself.
    #[prost(string, tag = "3")]
    path: String,
    /// A write's total size.
    #[prost(uint64, tag = "4")]
    size: u64,
    #[prost(bytes = "vec", tag = "5")]
    data: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct WireEntry {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(bool, tag = "2")]
    dir: bool,
    #[prost(uint64, tag = "3")]
    size: u64,
}

/// The owner's answers: refused (`error`), go ahead (`ready`), then the
/// result; a read's bytes come over several replies, the last one `done`.
#[derive(Clone, PartialEq, prost::Message)]
struct FolderReply {
    #[prost(string, tag = "1")]
    error: String,
    #[prost(bool, tag = "2")]
    ready: bool,
    #[prost(message, repeated, tag = "3")]
    entries: Vec<WireEntry>,
    #[prost(bytes = "vec", tag = "4")]
    data: Vec<u8>,
    #[prost(bool, tag = "5")]
    done: bool,
}

impl FolderReply {
    fn refused(error: impl ToString) -> Self {
        Self { error: error.to_string(), done: true, ..Self::default() }
    }
}

async fn recv<M: prost::Message + Default>(recv: &mut RecvHalf) -> Result<M> {
    recv_message(recv).await?.ok_or_else(|| NodeError::Protocol("the stream ended early".into()))
}

/// Serves folder streams on accepted connections.
pub(crate) struct Handler(pub(crate) Weak<Inner>);

#[async_trait]
impl ProtocolHandler for Handler {
    async fn handle(&self, conn: Connection) {
        while let Ok(stream) = conn.accept_stream().await {
            let Some(inner) = self.0.upgrade() else { return };
            let node = inner.node();
            let peer = conn.remote();
            tokio::spawn(async move {
                if let Err(error) = serve(&node, peer, stream).await {
                    tracing::debug!(%peer, %error, "folder stream ended");
                }
            });
        }
    }
}

async fn serve(node: &Node, peer: NodeId, mut stream: Stream) -> Result<()> {
    let request: FolderRequest = recv(&mut stream.recv).await?;
    let grants = challenge_grants(node, peer, &mut stream).await?;
    let Some(op) = Op::parse(&request.op) else {
        return send_message(&mut stream.send, &FolderReply::refused("unknown folder request")).await.map_err(Into::into);
    };
    if op == Op::Write && request.size > MAX_FILE {
        let error = format!("files over {} MiB can't be written over the peer link", MAX_FILE >> 20);
        return send_message(&mut stream.send, &FolderReply::refused(error)).await.map_err(Into::into);
    }
    let checker = node.clone();
    let (folder, path) = (request.folder.clone(), request.path.clone());
    let target = tokio::task::spawn_blocking(move || resolve(&checker, &grants, op, &folder, &path))
        .await
        .map_err(|error| NodeError::Protocol(error.to_string()))?;
    let target = match target {
        Ok(target) => target,
        Err(error) => {
            tracing::debug!(%peer, %error, op = op.as_str(), "folder request refused");
            return send_message(&mut stream.send, &FolderReply::refused(error)).await.map_err(Into::into);
        }
    };
    send_message(&mut stream.send, &FolderReply { ready: true, ..FolderReply::default() }).await?;
    match op {
        Op::List => {
            let reply = match blocking(move || list(&target)).await? {
                Ok(entries) => FolderReply { entries, done: true, ..FolderReply::default() },
                Err(error) => FolderReply::refused(error),
            };
            send_message(&mut stream.send, &reply).await?;
        }
        Op::Read => match blocking(move || read(&target)).await? {
            Err(error) => send_message(&mut stream.send, &FolderReply::refused(error)).await?,
            Ok(bytes) => {
                let mut chunks = bytes.chunks(CHUNK).peekable();
                if chunks.peek().is_none() {
                    send_message(&mut stream.send, &FolderReply { done: true, ..FolderReply::default() }).await?;
                }
                while let Some(chunk) = chunks.next() {
                    let done = chunks.peek().is_none();
                    send_message(&mut stream.send, &FolderReply { data: chunk.to_vec(), done, ..FolderReply::default() }).await?;
                }
            }
        },
        Op::Write => {
            // `size` is at most MAX_FILE, checked above.
            let size = request.size as usize;
            let mut bytes = Vec::with_capacity(size);
            while bytes.len() < size {
                let chunk: FolderRequest = recv(&mut stream.recv).await?;
                if chunk.data.is_empty() || bytes.len() + chunk.data.len() > size {
                    return send_message(&mut stream.send, &FolderReply::refused("the file's bytes don't match its size"))
                        .await
                        .map_err(Into::into);
                }
                bytes.extend_from_slice(&chunk.data);
            }
            let reply = match blocking(move || write(&target, &bytes)).await? {
                Ok(()) => FolderReply { done: true, ..FolderReply::default() },
                Err(error) => FolderReply::refused(error),
            };
            send_message(&mut stream.send, &reply).await?;
        }
    }
    Ok(())
}

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(work).await.map_err(|error| NodeError::Protocol(error.to_string()))
}

/// Whether one of `grants` allows `action` on `folder` now.
fn covered(node: &Node, grants: &[VerifiedGrant], action: Action, folder: &str) -> bool {
    let now = now_ms();
    let request = Request::folder(action, folder);
    grants.iter().any(|grant| {
        grant.expires_ms() > now
            && !grant.chain().iter().any(|link| node.grant_store().is_revoked(&link.grant))
            && grant.authorize(&request, now).is_ok()
    })
}

/// The real path `op` acts on: the requested folder must be covered by a
/// grant both as asked and after resolving links, and the relative path
/// must stay inside it.
fn resolve(node: &Node, grants: &[VerifiedGrant], op: Op, folder: &str, path: &str) -> std::result::Result<PathBuf, String> {
    let action = op.action();
    if !covered(node, grants, action, folder) {
        return Err(format!("no grant lets this node {} {folder}", if action == Action::Write { "write to" } else { "read" }));
    }
    let root = loa_exec::canonicalize(folder).map_err(|error| format!("{folder} can't be opened: {error}"))?;
    let real = root.to_str().ok_or_else(|| format!("{folder} isn't a UTF-8 path"))?;
    if !covered(node, grants, action, real) {
        return Err(format!("{folder} leads outside the shared folder"));
    }
    confine(&root, path, op == Op::Write)
}

/// `relative` inside `root` (a canonical folder). Reads resolve links and
/// must land inside `root`; a write's folder must, and it never writes
/// through a link or over a folder.
pub(crate) fn confine(root: &Path, relative: &str, write: bool) -> std::result::Result<PathBuf, String> {
    let outside = || format!("{relative} is outside the shared folder");
    let mut joined = root.to_path_buf();
    let mut name = None;
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => {
                joined.push(part);
                name = Some(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return Err(outside()),
        }
    }
    if !write {
        let real = loa_exec::canonicalize(&joined).map_err(|error| format!("{relative}: {error}"))?;
        return if real.starts_with(root) { Ok(real) } else { Err(outside()) };
    }
    let name = name.ok_or_else(|| "name a file to write".to_owned())?;
    let parent = joined.parent().unwrap_or(root);
    let parent = loa_exec::canonicalize(parent).map_err(|error| format!("{relative}: {error}"))?;
    if !parent.starts_with(root) {
        return Err(outside());
    }
    let target = parent.join(name);
    match std::fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => Err(format!("{relative} is a link; links aren't written through")),
        Ok(meta) if meta.is_dir() => Err(format!("{relative} is a folder")),
        _ => Ok(target),
    }
}

fn list(dir: &Path) -> std::result::Result<Vec<WireEntry>, String> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        // Names that aren't UTF-8 couldn't be asked for again.
        let Ok(name) = entry.file_name().into_string() else { continue };
        let path = entry.path();
        let meta = std::fs::metadata(&path).or_else(|_| std::fs::symlink_metadata(&path)).map_err(|error| error.to_string())?;
        entries.push(WireEntry { name, dir: meta.is_dir(), size: if meta.is_dir() { 0 } else { meta.len() } });
    }
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

fn read(path: &Path) -> std::result::Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !meta.is_file() {
        return Err("not a file".into());
    }
    if meta.len() > MAX_FILE {
        return Err(format!("files over {} MiB can't be opened over the peer link", MAX_FILE >> 20));
    }
    std::fs::read(path).map_err(|error| error.to_string())
}

/// Writes through a new file next to `target`, then renames it over
/// `target`: a link put there meanwhile is replaced, never followed.
fn write(target: &Path, bytes: &[u8]) -> std::result::Result<(), String> {
    use std::io::Write as _;
    let name = target.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let part = target.with_file_name(format!(".{name}.part-{}", now_ms()));
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&part, target));
    if result.is_err() {
        let _ = std::fs::remove_file(&part);
    }
    result.map_err(|error| error.to_string())
}

/// The requester side: a stream past the challenge and the owner's go-ahead,
/// with a write's bytes sent.
async fn request(node: &Node, peer: NodeId, request: FolderRequest, upload: &[u8]) -> Result<(Connection, Stream)> {
    let conn = node.dial(peer, PROTOCOL).await?;
    let mut stream = conn.open_stream().await?;
    send_message(&mut stream.send, &request).await?;
    present_grants(node, peer, &mut stream).await?;
    let answer: FolderReply = recv(&mut stream.recv).await?;
    if !answer.error.is_empty() {
        conn.close(0, b"done");
        return Err(NodeError::Refused(answer.error));
    }
    if !answer.ready {
        conn.close(0, b"done");
        return Err(NodeError::Protocol("expected the owner's go-ahead".into()));
    }
    for chunk in upload.chunks(CHUNK) {
        send_message(&mut stream.send, &FolderRequest { data: chunk.to_vec(), ..FolderRequest::default() }).await?;
    }
    Ok((conn, stream))
}

fn opening(op: Op, folder: &str, path: &str) -> FolderRequest {
    FolderRequest { op: op.as_str().to_owned(), folder: folder.to_owned(), path: path.to_owned(), ..FolderRequest::default() }
}

pub(crate) async fn list_remote(node: &Node, peer: NodeId, folder: &str, path: &str) -> Result<Vec<FolderEntry>> {
    let (conn, mut stream) = request(node, peer, opening(Op::List, folder, path), &[]).await?;
    let reply: Result<FolderReply> = recv(&mut stream.recv).await;
    conn.close(0, b"done");
    let reply = reply?;
    if !reply.error.is_empty() {
        return Err(NodeError::Refused(reply.error));
    }
    Ok(reply.entries.into_iter().map(|entry| FolderEntry { name: entry.name, dir: entry.dir, size: entry.size }).collect())
}

pub(crate) async fn read_remote(node: &Node, peer: NodeId, folder: &str, path: &str) -> Result<Vec<u8>> {
    let (conn, mut stream) = request(node, peer, opening(Op::Read, folder, path), &[]).await?;
    let mut bytes = Vec::new();
    let result = loop {
        let reply: FolderReply = match recv(&mut stream.recv).await {
            Ok(reply) => reply,
            Err(error) => break Err(error),
        };
        if !reply.error.is_empty() {
            break Err(NodeError::Refused(reply.error));
        }
        bytes.extend_from_slice(&reply.data);
        if bytes.len() as u64 > MAX_FILE {
            break Err(NodeError::Protocol("the file is larger than the owner said".into()));
        }
        if reply.done {
            break Ok(bytes);
        }
    };
    conn.close(0, b"done");
    result
}

pub(crate) async fn write_remote(node: &Node, peer: NodeId, folder: &str, path: &str, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(NodeError::Refused(format!("files over {} MiB can't be written over the peer link", MAX_FILE >> 20)));
    }
    let mut opening = opening(Op::Write, folder, path);
    opening.size = bytes.len() as u64;
    let (conn, mut stream) = request(node, peer, opening, bytes).await?;
    let reply: Result<FolderReply> = recv(&mut stream.recv).await;
    conn.close(0, b"done");
    let reply = reply?;
    if reply.error.is_empty() { Ok(()) } else { Err(NodeError::Refused(reply.error)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = loa_exec::canonicalize(dir.path()).unwrap().join("share");
        let outside = root.parent().unwrap().join("secret");
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(root.join("docs/a.txt"), "a").unwrap();
        std::fs::write(outside.join("key"), "k").unwrap();

        assert_eq!(confine(&root, "", false).unwrap(), root);
        assert_eq!(confine(&root, "docs/./a.txt", false).unwrap(), root.join("docs/a.txt"));
        for escape in ["..", "../secret/key", "docs/../../secret", "/etc/passwd"] {
            assert!(confine(&root, escape, false).is_err(), "{escape}");
            assert!(confine(&root, escape, true).is_err(), "{escape}");
        }
        assert!(confine(&root, "missing.txt", false).is_err());
        assert_eq!(confine(&root, "docs/new.txt", true).unwrap(), root.join("docs/new.txt"));
        assert!(confine(&root, "", true).is_err(), "the folder itself isn't a file");
        assert!(confine(&root, "docs", true).is_err(), "no writing over a folder");

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&outside, root.join("out")).unwrap();
            symlink(outside.join("key"), root.join("key-link")).unwrap();
            symlink(root.join("docs/a.txt"), root.join("inside-link")).unwrap();
            assert!(confine(&root, "out", false).is_err());
            assert!(confine(&root, "out/key", false).is_err());
            assert!(confine(&root, "key-link", false).is_err());
            assert!(confine(&root, "out/new.txt", true).is_err(), "a linked folder outside isn't written");
            assert!(confine(&root, "key-link", true).is_err(), "links aren't written through");
            assert_eq!(confine(&root, "inside-link", false).unwrap(), root.join("docs/a.txt"));
        }

        write(&root.join("docs/new.txt"), b"fresh").unwrap();
        assert_eq!(std::fs::read(root.join("docs/new.txt")).unwrap(), b"fresh");
        let names: Vec<String> = list(&root.join("docs")).unwrap().into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["a.txt", "new.txt"], "no part files left");
    }
}
