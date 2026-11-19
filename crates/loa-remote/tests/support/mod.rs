//! In-process SSH server (russh) with an in-memory SFTP file system, an exec
//! handler and direct-tcpip forwarding, plus a fake ssh-agent on a temp unix
//! socket. Everything binds loopback port 0 or a temp dir.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use loa_remote::audit::AuditLog;
use loa_remote::connection::ConnectorConfig;
use loa_remote::hosts::{Host, HostRegistry};
use loa_remote::ports::{Approval, ApprovalRequest, Approver, HostKeyDecider, HostKeyPrompt};
use loa_remote::{RemoteService, RemoteServiceConfig};
use russh::keys::agent::client::AgentClient;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId, Sig};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode,
    Version,
};

pub const USER: &str = "tester";

pub fn random_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

// ---------------------------------------------------------------- memory fs

#[derive(Debug, Clone)]
pub enum Node {
    Dir,
    File { data: Vec<u8>, mode: u32 },
    Symlink(String),
}

/// A tiny POSIX-like file system keyed by absolute path.
#[derive(Debug, Default)]
pub struct MemFs {
    pub nodes: BTreeMap<String, Node>,
}

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

impl MemFs {
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert("/".to_string(), Node::Dir);
        Self { nodes }
    }

    pub fn mkdir(&mut self, path: &str) {
        self.nodes.insert(path.to_string(), Node::Dir);
    }

    pub fn put(&mut self, path: &str, data: &[u8]) {
        self.nodes.insert(
            path.to_string(),
            Node::File {
                data: data.to_vec(),
                mode: 0o640,
            },
        );
    }

    pub fn symlink(&mut self, path: &str, target: &str) {
        self.nodes
            .insert(path.to_string(), Node::Symlink(target.to_string()));
    }

    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        match self.nodes.get(path) {
            Some(Node::File { data, .. }) => Some(data.clone()),
            _ => None,
        }
    }

    /// Resolves every symlink in `path`; the result must exist.
    pub fn resolve(&self, path: &str) -> Result<String, StatusCode> {
        let mut out = String::new();
        let mut parts: Vec<String> = path
            .split('/')
            .filter(|p| !p.is_empty())
            .rev()
            .map(str::to_string)
            .collect();
        let mut hops = 0;
        while let Some(part) = parts.pop() {
            match part.as_str() {
                "." => continue,
                ".." => {
                    out = parent_of(if out.is_empty() { "/" } else { &out }).to_string();
                    if out == "/" {
                        out.clear();
                    }
                    continue;
                }
                _ => {}
            }
            let candidate = format!("{out}/{part}");
            match self.nodes.get(&candidate) {
                None => return Err(StatusCode::NoSuchFile),
                Some(Node::Symlink(target)) => {
                    hops += 1;
                    if hops > 32 {
                        return Err(StatusCode::Failure);
                    }
                    if target.starts_with('/') {
                        out.clear();
                    }
                    parts.extend(
                        target
                            .split('/')
                            .filter(|p| !p.is_empty())
                            .rev()
                            .map(str::to_string),
                    );
                }
                Some(_) => out = candidate,
            }
        }
        Ok(if out.is_empty() { "/".to_string() } else { out })
    }

    fn attrs(node: &Node) -> FileAttributes {
        let mut a = FileAttributes::empty();
        match node {
            Node::Dir => {
                a.permissions = Some(0o755);
                a.set_dir(true);
            }
            Node::File { data, mode } => {
                a.permissions = Some(*mode);
                a.set_regular(true);
                a.size = Some(data.len() as u64);
            }
            Node::Symlink(_) => {
                a.permissions = Some(0o777);
                a.set_symlink(true);
            }
        }
        a
    }
}

// --------------------------------------------------------------- sftp side

struct OpenFile {
    path: String,
    dir_done: bool,
}

struct MemSftp {
    fs: Arc<Mutex<MemFs>>,
    handles: HashMap<String, OpenFile>,
    next: usize,
    posix_rename: bool,
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".into(),
        language_tag: "en-US".into(),
    }
}

impl MemSftp {
    fn handle(&mut self, path: String) -> String {
        self.next += 1;
        let h = format!("h{}", self.next);
        self.handles.insert(
            h.clone(),
            OpenFile {
                path,
                dir_done: false,
            },
        );
        h
    }

    /// Follows a symlink at the last component for open/stat.
    fn follow(&self, path: &str) -> Result<String, StatusCode> {
        let fs = self.fs.lock().unwrap();
        match fs.nodes.get(path) {
            Some(Node::Symlink(_)) => fs.resolve(path),
            _ => Ok(path.to_string()),
        }
    }
}

impl russh_sftp::server::Handler for MemSftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        let mut v = Version::new();
        if self.posix_rename {
            v.extensions
                .insert("posix-rename@openssh.com".into(), "1".into());
        }
        Ok(v)
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let path = self.follow(&filename)?;
        {
            let mut fs = self.fs.lock().unwrap();
            if !matches!(fs.nodes.get(parent_of(&path)), Some(Node::Dir)) {
                return Err(StatusCode::NoSuchFile);
            }
            match fs.nodes.get_mut(&path) {
                Some(Node::Dir) => return Err(StatusCode::Failure),
                Some(Node::File { data, .. }) => {
                    if pflags.contains(OpenFlags::EXCLUDE) {
                        return Err(StatusCode::Failure);
                    }
                    if pflags.contains(OpenFlags::TRUNCATE) {
                        data.clear();
                    }
                }
                Some(Node::Symlink(_)) => return Err(StatusCode::NoSuchFile),
                None if pflags.contains(OpenFlags::CREATE) => {
                    fs.nodes.insert(
                        path.clone(),
                        Node::File {
                            data: Vec::new(),
                            mode: attrs.permissions.unwrap_or(0o644),
                        },
                    );
                }
                None => return Err(StatusCode::NoSuchFile),
            }
        }
        Ok(Handle {
            id,
            handle: self.handle(path),
        })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.remove(&handle);
        Ok(ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let path = &self.handles.get(&handle).ok_or(StatusCode::Failure)?.path;
        let fs = self.fs.lock().unwrap();
        let Some(Node::File { data, .. }) = fs.nodes.get(path) else {
            return Err(StatusCode::NoSuchFile);
        };
        let start = offset as usize;
        if start >= data.len() {
            return Err(StatusCode::Eof);
        }
        let end = (start + len as usize).min(data.len());
        Ok(Data {
            id,
            data: data[start..end].to_vec(),
        })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        bytes: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let path = self
            .handles
            .get(&handle)
            .ok_or(StatusCode::Failure)?
            .path
            .clone();
        let mut fs = self.fs.lock().unwrap();
        let Some(Node::File { data, .. }) = fs.nodes.get_mut(&path) else {
            return Err(StatusCode::NoSuchFile);
        };
        let start = offset as usize;
        if data.len() < start + bytes.len() {
            data.resize(start + bytes.len(), 0);
        }
        data[start..start + bytes.len()].copy_from_slice(&bytes);
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let fs = self.fs.lock().unwrap();
        let node = fs.nodes.get(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Attrs {
            id,
            attrs: MemFs::attrs(node),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let fs = self.fs.lock().unwrap();
        let real = fs.resolve(&path)?;
        let node = fs.nodes.get(&real).ok_or(StatusCode::NoSuchFile)?;
        Ok(Attrs {
            id,
            attrs: MemFs::attrs(node),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let path = self
            .handles
            .get(&handle)
            .ok_or(StatusCode::Failure)?
            .path
            .clone();
        self.lstat(id, path).await
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let real = self.fs.lock().unwrap().resolve(&path)?;
        if !matches!(self.fs.lock().unwrap().nodes.get(&real), Some(Node::Dir)) {
            return Err(StatusCode::NoSuchFile);
        }
        Ok(Handle {
            id,
            handle: self.handle(real),
        })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let open = self.handles.get_mut(&handle).ok_or(StatusCode::Failure)?;
        if open.dir_done {
            return Err(StatusCode::Eof);
        }
        open.dir_done = true;
        let dir = open.path.clone();
        let fs = self.fs.lock().unwrap();
        let files = fs
            .nodes
            .iter()
            .filter(|(p, _)| p.as_str() != "/" && parent_of(p) == dir)
            .map(|(p, n)| File {
                filename: p.rsplit('/').next().unwrap().to_string(),
                longname: String::new(),
                attrs: MemFs::attrs(n),
            })
            .collect();
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let mut fs = self.fs.lock().unwrap();
        match fs.nodes.get(&filename) {
            Some(Node::Dir) | None => Err(StatusCode::NoSuchFile),
            Some(_) => {
                fs.nodes.remove(&filename);
                Ok(ok(id))
            }
        }
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let real = self.fs.lock().unwrap().resolve(&path)?;
        Ok(Name {
            id,
            files: vec![File::dummy(real)],
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        let mut fs = self.fs.lock().unwrap();
        if fs.nodes.contains_key(&newpath) {
            // SFTPv3 / OpenSSH: plain rename refuses to overwrite.
            return Err(StatusCode::Failure);
        }
        let node = fs.nodes.remove(&oldpath).ok_or(StatusCode::NoSuchFile)?;
        fs.nodes.insert(newpath, node);
        Ok(ok(id))
    }

    async fn extended(
        &mut self,
        id: u32,
        request: String,
        data: Vec<u8>,
    ) -> Result<Packet, Self::Error> {
        if request != "posix-rename@openssh.com" || !self.posix_rename {
            return Err(StatusCode::OpUnsupported);
        }
        let mut rest = data.as_slice();
        let mut take = || {
            let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
            let s = String::from_utf8(rest[4..4 + len].to_vec()).unwrap();
            rest = &rest[4 + len..];
            s
        };
        let (from, to) = (take(), take());
        let mut fs = self.fs.lock().unwrap();
        let node = fs.nodes.remove(&from).ok_or(StatusCode::NoSuchFile)?;
        fs.nodes.insert(to, node);
        Ok(Packet::Status(ok(id)))
    }
}

// ---------------------------------------------------------------- ssh side

/// What the server saw, for assertions.
#[derive(Default)]
pub struct Stats {
    pub connections: AtomicUsize,
    pub commands: Mutex<Vec<String>>,
    pub signals: Mutex<Vec<String>>,
    pub forwards: Mutex<Vec<(String, u32)>>,
    /// Signalled each time an exec request arrives.
    pub command_seen: tokio::sync::Notify,
}

#[derive(Clone)]
struct Shared {
    authorized: PublicKey,
    fs: Arc<Mutex<MemFs>>,
    stats: Arc<Stats>,
    posix_rename: bool,
}

struct ConnHandler {
    shared: Shared,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

impl russh::server::Handler for ConnHandler {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        if user == USER && key.public_key.key_data() == self.shared.authorized.key_data() {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.shared
            .stats
            .forwards
            .lock()
            .unwrap()
            .push((host_to_connect.to_string(), port_to_connect));
        match tokio::net::TcpStream::connect((host_to_connect, port_to_connect as u16)).await {
            Ok(mut tcp) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
                });
            }
            Err(_) => {
                reply
                    .reject(russh::ChannelOpenFailure::ConnectFailed)
                    .await;
            }
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        self.shared.stats.commands.lock().unwrap().push(command.clone());
        self.shared.stats.command_seen.notify_one();
        session.channel_success(channel)?;
        let handle = session.handle();
        tokio::spawn(async move {
            let (stdout, stderr, status): (Vec<u8>, &[u8], u32) =
                if let Some(rest) = command.strip_prefix("echo ") {
                    (format!("{rest}\n").into_bytes(), b"", 0)
                } else if command == "fail" {
                    (Vec::new(), b"boom\n", 3)
                } else if let Some(n) = command.strip_prefix("big ") {
                    (vec![b'x'; n.parse().unwrap()], b"", 0)
                } else if command == "hang" {
                    return;
                } else {
                    (format!("ran: {command}\n").into_bytes(), b"", 0)
                };
            let _ = handle.data(channel, stdout).await;
            if !stderr.is_empty() {
                let _ = handle.extended_data(channel, 1, stderr.to_vec()).await;
            }
            let _ = handle.exit_status_request(channel, status).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }

    async fn signal(
        &mut self,
        _channel: ChannelId,
        signal: Sig,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.shared
            .stats
            .signals
            .lock()
            .unwrap()
            .push(format!("{signal:?}"));
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        match (name, self.channels.remove(&channel_id)) {
            ("sftp", Some(channel)) => {
                session.channel_success(channel_id)?;
                let sftp = MemSftp {
                    fs: self.shared.fs.clone(),
                    handles: HashMap::new(),
                    next: 0,
                    posix_rename: self.shared.posix_rename,
                };
                russh_sftp::server::run(channel.into_stream(), sftp).await;
            }
            _ => session.channel_failure(channel_id)?,
        }
        Ok(())
    }
}

/// A running in-process SSH server.
pub struct TestServer {
    pub addr: SocketAddr,
    pub host_key: PrivateKey,
    pub fs: Arc<Mutex<MemFs>>,
    pub stats: Arc<Stats>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestServer {
    pub async fn start(authorized: &PublicKey) -> Self {
        Self::start_with(authorized, true).await
    }

    pub async fn start_with(authorized: &PublicKey, posix_rename: bool) -> Self {
        let host_key = random_key();
        let config = Arc::new(russh::server::Config {
            keys: vec![host_key.clone()],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            inactivity_timeout: None,
            ..Default::default()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut fs = MemFs::new();
        fs.mkdir("/srv");
        let shared = Shared {
            authorized: authorized.clone(),
            fs: Arc::new(Mutex::new(fs)),
            stats: Arc::new(Stats::default()),
            posix_rename,
        };
        let (fs, stats) = (shared.fs.clone(), shared.stats.clone());
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                shared.stats.connections.fetch_add(1, Ordering::SeqCst);
                let handler = ConnHandler {
                    shared: shared.clone(),
                    channels: HashMap::new(),
                };
                let config = config.clone();
                tokio::spawn(async move {
                    if let Ok(running) = russh::server::run_stream(config, tcp, handler).await {
                        let _ = running.await;
                    }
                });
            }
        });
        Self {
            addr,
            host_key,
            fs,
            stats,
            task,
        }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The `known_hosts` line for this server's real key.
    pub fn known_hosts_line(&self) -> String {
        format!(
            "[127.0.0.1]:{} {}\n",
            self.port(),
            self.host_key.public_key().to_openssh().unwrap()
        )
    }

    pub fn connections(&self) -> usize {
        self.stats.connections.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------- agent

/// A fake ssh-agent on a temp unix socket, holding one generated key.
pub struct FakeAgent {
    pub socket: PathBuf,
    pub key: PrivateKey,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeAgent {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeAgent {
    pub async fn start(dir: &Path) -> Self {
        Self::start_with_key(dir, random_key()).await
    }

    pub async fn start_with_key(dir: &Path, key: PrivateKey) -> Self {
        let socket = dir.join(format!("agent-{}.sock", rand::random::<u32>()));
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let stream = tokio_stream::wrappers::UnixListenerStream::new(listener);
        let task = tokio::spawn(async move {
            let _ = russh::keys::agent::server::serve(stream, ()).await;
        });
        let mut client = AgentClient::connect_uds(&socket).await.unwrap();
        client.add_identity(&key, &[]).await.unwrap();
        Self { socket, key, task }
    }

    pub fn public(&self) -> PublicKey {
        self.key.public_key().clone()
    }
}

// ---------------------------------------------------------------- ports

/// Records prompts and answers with a fixed decision.
pub struct RecordingDecider {
    pub accept: bool,
    pub prompts: Mutex<Vec<HostKeyPrompt>>,
}

impl RecordingDecider {
    pub fn new(accept: bool) -> Arc<Self> {
        Arc::new(Self {
            accept,
            prompts: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl HostKeyDecider for RecordingDecider {
    async fn decide(&self, prompt: HostKeyPrompt) -> bool {
        self.prompts.lock().unwrap().push(prompt);
        self.accept
    }
}

/// Answers approvals from a queue (default: deny) and records requests.
pub struct ScriptedApprover {
    pub answers: Mutex<Vec<Approval>>,
    pub requests: Mutex<Vec<ApprovalRequest>>,
}

impl ScriptedApprover {
    pub fn new(answers: Vec<Approval>) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().rev().collect()),
            requests: Mutex::new(Vec::new()),
        })
    }

    pub fn asked(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

#[async_trait]
impl Approver for ScriptedApprover {
    async fn approve(&self, request: ApprovalRequest) -> Approval {
        self.requests.lock().unwrap().push(request);
        self.answers
            .lock()
            .unwrap()
            .pop()
            .unwrap_or(Approval::Deny { reason: None })
    }
}

// ---------------------------------------------------------------- harness

/// A service wired to temp known_hosts, an in-memory audit log, and the
/// given agent.
pub struct Harness {
    pub dir: tempfile::TempDir,
    pub known_hosts: PathBuf,
    pub hosts: Arc<HostRegistry>,
    pub audit: Arc<AuditLog>,
    pub decider: Arc<RecordingDecider>,
    pub approver: Arc<ScriptedApprover>,
    pub service: Arc<RemoteService>,
}

impl Harness {
    pub fn new(
        dir: tempfile::TempDir,
        agent: Option<&FakeAgent>,
        known_hosts_text: &str,
        accept_keys: bool,
        approvals: Vec<Approval>,
    ) -> Self {
        let known_hosts = dir.path().join("known_hosts");
        std::fs::write(&known_hosts, known_hosts_text).unwrap();
        let hosts = Arc::new(HostRegistry::new());
        let audit = Arc::new(AuditLog::in_memory().unwrap());
        let decider = RecordingDecider::new(accept_keys);
        let approver = ScriptedApprover::new(approvals);
        let service = Arc::new(RemoteService::new(
            hosts.clone(),
            audit.clone(),
            decider.clone(),
            approver.clone(),
            RemoteServiceConfig {
                known_hosts: vec![known_hosts.clone()],
                known_hosts_write: known_hosts.clone(),
                connector: ConnectorConfig::with_agent(agent.map(|a| a.socket.clone())),
            },
        ));
        Self {
            dir,
            known_hosts,
            hosts,
            audit,
            decider,
            approver,
            service,
        }
    }

    /// Registers `name` pointing at `server` directly.
    pub fn add(&self, name: &str, server: &TestServer) -> Host {
        let host = Host::manual(name, "127.0.0.1", Some(server.port()), USER);
        self.hosts.insert(host.clone());
        host
    }
}
