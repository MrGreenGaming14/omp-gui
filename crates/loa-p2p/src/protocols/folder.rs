//! `omp/folder/1`: folders shared through folder grants. A read grant lists
//! and reads, a write grant also writes; every path stays inside the folder.
//! One request per stream: the request, the grant challenge, the owner's
//! go-ahead, then a write's bytes or a read's chunks.

use crate::auth::GrantGate;
use crate::node::Node;
use crate::transport::{Connection, Stream};
use crate::wire::{self, Envelope, FolderReply, FolderRequest, WireEntry};
use crate::{NodeError, NodeId, Result};
use loa_trust::{Action, PublicKey, Request};
use loa_util::time::now_ms;
use std::path::{Path, PathBuf};

/// Largest file read or written over the protocol.
pub const MAX_FILE: u64 = 16 * 1024 * 1024;
/// File bytes per frame, well under the frame limit.
const CHUNK: usize = 512 * 1024;

/// One entry of a shared folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

/// A folder of a peer this node's grants reach.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedFolder {
    pub peer: NodeId,
    /// Absolute on the peer.
    pub path: String,
    pub can_write: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    List,
    Read,
    Write,
}

const OPS: [(Op, &str); 3] = [(Op::List, "list"), (Op::Read, "read"), (Op::Write, "write")];

fn too_big(verb: &str) -> String { format!("files over {} MiB can't be {verb} over the peer link", MAX_FILE >> 20) }

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(work).await.map_err(|e| e.to_string())?
}

fn reply(error: String) -> FolderReply { FolderReply::ack(Some(error)) }

pub(crate) async fn serve(node: Node, conn: Connection, mut stream: Stream) -> Result<()> {
    let request: FolderRequest = wire::recv(&mut stream.recv).await?.ok_or_else(|| NodeError::Protocol("no request".into()))?;
    node.0.grants.challenge(conn.remote(), &mut stream).await?;
    let peer = PublicKey::from(conn.remote()).into();
    let op = OPS.iter().find(|(_, name)| *name == request.op).map(|(op, _)| *op);
    let checked = match op {
        None => Err("unknown folder request".to_owned()),
        Some(Op::Write) if request.size > MAX_FILE => Err(too_big("written")),
        Some(op) => {
            let (access, action) = (node.0.grants.access.clone(), if op == Op::Write { Action::Write } else { Action::Read });
            let covered = move |path: &str| access.allows(&peer, &Request::folder(action, path), now_ms());
            let (folder, path) = (request.folder.clone(), request.path.clone());
            blocking(move || resolve(&covered, op == Op::Write, &folder, &path)).await.map(|target| (op, target))
        }
    };
    let tx = &mut stream.send;
    let (op, target) = match checked {
        Ok(checked) => checked,
        Err(error) => return wire::send(tx, &reply(error)).await,
    };
    wire::send(tx, &FolderReply::ack(None)).await?;
    let done = |entries, data| FolderReply { entries, data, done: true, ..FolderReply::default() };
    match op {
        Op::List => wire::send(tx, &blocking(move || list(&target)).await.map_or_else(reply, |entries| done(entries, Vec::new()))).await,
        Op::Read => match blocking(move || read(&target)).await {
            Err(error) => wire::send(tx, &reply(error)).await,
            Ok(bytes) if bytes.is_empty() => wire::send(tx, &done(Vec::new(), Vec::new())).await,
            Ok(bytes) => {
                let mut chunks = bytes.chunks(CHUNK).peekable();
                while let Some(chunk) = chunks.next() {
                    wire::send(tx, &FolderReply { data: chunk.to_vec(), done: chunks.peek().is_none(), ..FolderReply::default() }).await?;
                }
                Ok(())
            }
        },
        Op::Write => {
            let size = request.size as usize;
            let mut bytes = Vec::with_capacity(size);
            while bytes.len() < size {
                let chunk: FolderRequest = wire::recv(&mut stream.recv).await?.unwrap_or_default();
                if chunk.data.is_empty() || bytes.len() + chunk.data.len() > size {
                    return wire::send(&mut stream.send, &reply("the file's bytes don't match its size".into())).await;
                }
                bytes.extend_from_slice(&chunk.data);
            }
            let written = blocking(move || loa_util::fs::write_atomic(&target, &bytes, None).map_err(|e| e.to_string())).await;
            wire::send(&mut stream.send, &written.map_or_else(reply, |()| done(Vec::new(), Vec::new()))).await
        }
    }
}

/// The real path an op acts on: the folder must be covered as asked and once
/// links are resolved, and the path must stay inside it.
fn resolve(covered: &dyn Fn(&str) -> bool, write: bool, folder: &str, path: &str) -> Result<PathBuf, String> {
    let verb = if write { "write to" } else { "read" };
    if !covered(folder) {
        return Err(format!("no grant lets this node {verb} {folder}"));
    }
    let root = std::fs::canonicalize(folder).map_err(|e| format!("{folder} can't be opened: {e}"))?;
    if !root.to_str().is_some_and(covered) {
        return Err(format!("{folder} leads outside the shared folder"));
    }
    let target = loa_util::fs::confine(&root, Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    match (write, std::fs::metadata(&target)) {
        (true, _) if target == root => Err("name a file to write".into()),
        (true, Ok(meta)) if meta.is_dir() => Err(format!("{path} is a folder")),
        (false, Err(e)) => Err(format!("{path}: {e}")),
        _ => Ok(target),
    }
}

fn list(dir: &Path) -> Result<Vec<WireEntry>, String> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        // Names that aren't UTF-8 couldn't be asked for again.
        let Ok(name) = entry.file_name().into_string() else { continue };
        let meta = std::fs::metadata(entry.path()).or_else(|_| entry.metadata()).map_err(|e| e.to_string())?;
        entries.push(WireEntry { name, dir: meta.is_dir(), size: if meta.is_dir() { 0 } else { meta.len() } });
    }
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    match () {
        _ if !meta.is_file() => Err("not a file".into()),
        _ if meta.len() > MAX_FILE => Err(too_big("opened")),
        _ => std::fs::read(path).map_err(|e| e.to_string()),
    }
}

/// The requester side: one op on a fresh stream; `on_reply` takes each reply until it says done.
async fn request(node: &Node, peer: NodeId, op: &str, folder: &str, path: &str, upload: &[u8], mut on_reply: impl FnMut(FolderReply) -> Result<bool>) -> Result<()> {
    let conn = node.dial(peer, wire::FOLDER).await?;
    let result = async {
        let mut stream = conn.open_stream().await?;
        let opening = FolderRequest { op: op.into(), folder: folder.into(), path: path.into(), size: upload.len() as u64, data: Vec::new() };
        wire::send(&mut stream.send, &opening).await?;
        node.0.grants.present(peer, &mut stream).await?;
        if !wire::expect::<FolderReply>(&mut stream.recv).await?.ready {
            return Err(NodeError::Protocol("expected the owner's go-ahead".into()));
        }
        for chunk in upload.chunks(CHUNK) {
            wire::send(&mut stream.send, &FolderRequest { data: chunk.to_vec(), ..FolderRequest::default() }).await?;
        }
        while !on_reply(wire::expect::<FolderReply>(&mut stream.recv).await?)? {}
        Ok(())
    }
    .await;
    conn.close(0, b"done");
    result
}

pub(crate) async fn list_remote(node: &Node, peer: NodeId, folder: &str, path: &str) -> Result<Vec<FolderEntry>> {
    let mut out = Vec::new();
    request(node, peer, "list", folder, path, &[], |reply| {
        out.extend(reply.entries.into_iter().map(|e| FolderEntry { name: e.name, dir: e.dir, size: e.size }));
        Ok(true)
    })
    .await?;
    Ok(out)
}

pub(crate) async fn read_remote(node: &Node, peer: NodeId, folder: &str, path: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    request(node, peer, "read", folder, path, &[], |reply| {
        bytes.extend_from_slice(&reply.data);
        if bytes.len() as u64 > MAX_FILE {
            return Err(NodeError::Protocol("the file is larger than the owner said".into()));
        }
        Ok(reply.done)
    })
    .await?;
    Ok(bytes)
}

pub(crate) async fn write_remote(node: &Node, peer: NodeId, folder: &str, path: &str, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 > MAX_FILE {
        return Err(NodeError::Refused(too_big("written")));
    }
    request(node, peer, "write", folder, path, bytes, |_| Ok(true)).await
}
