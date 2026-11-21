//! In-process russh server with an in-memory SFTP file system, exec, PTY shells and direct-tcpip,
//! a fake ssh-agent, recording ports and the [`setup`] builder. Binds loopback port 0 only.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use loa_remote::audit::MemoryAudit;
use loa_remote::connection::ConnectorConfig;
use loa_remote::hosts::{AgentSocket, Endpoint, Host, HostRegistry};
use loa_remote::limits::HostLimits;
use loa_remote::ports::{Approval, ApprovalRequest, Approver, HostKeyDecider, HostKeyPrompt};
use loa_remote::{RemoteService, RemoteServiceConfig};
use russh::keys::agent::client::AgentClient;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId, Sig};
use russh_sftp::extensions::HardlinkExtension;
use russh_sftp::protocol::{Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode, Version};

pub const USER: &str = "tester";

pub fn random_key() -> PrivateKey {
    PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Dir,
    File(Vec<u8>, u32),
    Symlink(String),
}

/// A tiny POSIX-like file system keyed by absolute path.
#[derive(Debug)]
pub struct MemFs(pub BTreeMap<String, Node>);

fn parent(path: &str) -> &str {
    path.rfind('/').map_or("/", |i| if i == 0 { "/" } else { &path[..i] })
}

impl MemFs {
    pub fn put(&mut self, path: &str, data: &[u8]) {
        self.0.insert(path.into(), Node::File(data.to_vec(), 0o640));
    }

    pub fn link(&mut self, path: &str, target: &str) {
        self.0.insert(path.into(), Node::Symlink(target.into()));
    }

    pub fn get(&self, path: &str) -> Option<&[u8]> {
        match self.0.get(path) {
            Some(Node::File(data, _)) => Some(data),
            _ => None,
        }
    }

    /// Resolves every symlink in `path`; the result must exist.
    fn resolve(&self, path: &str) -> Result<String, StatusCode> {
        let (mut out, mut hops) = (String::new(), 0);
        let mut parts: Vec<String> = path.split('/').filter(|p| !p.is_empty()).rev().map(Into::into).collect();
        while let Some(part) = parts.pop() {
            let candidate = match part.as_str() {
                "." => continue,
                ".." => parent(&out).trim_end_matches('/').to_owned(),
                _ => format!("{out}/{part}"),
            };
            match self.0.get(if candidate.is_empty() { "/" } else { &candidate }) {
                None => return Err(StatusCode::NoSuchFile),
                Some(Node::Symlink(target)) if hops < 32 => {
                    hops += 1;
                    if target.starts_with('/') {
                        out.clear();
                    }
                    parts.extend(target.split('/').filter(|p| !p.is_empty()).rev().map(Into::into));
                }
                Some(Node::Symlink(_)) => return Err(StatusCode::Failure),
                Some(_) => out = candidate,
            }
        }
        Ok(if out.is_empty() { "/".into() } else { out })
    }

    fn attrs(&self, path: &str) -> Result<FileAttributes, StatusCode> {
        let mut a = FileAttributes::empty();
        let node = self.0.get(path).ok_or(StatusCode::NoSuchFile)?;
        a.permissions = Some(match node {
            Node::Dir => 0o755,
            Node::File(data, mode) => (a.size = Some(data.len() as u64), *mode).1,
            Node::Symlink(_) => 0o777,
        });
        match node {
            Node::Dir => a.set_dir(true),
            Node::File(..) => a.set_regular(true),
            Node::Symlink(_) => a.set_symlink(true),
        }
        Ok(a)
    }
}

struct MemSftp {
    fs: Arc<Mutex<MemFs>>,
    handles: HashMap<String, (String, bool)>,
    posix_rename: bool,
}

fn ok(id: u32) -> Status {
    Status { id, status_code: StatusCode::Ok, error_message: "Ok".into(), language_tag: "en-US".into() }
}

impl MemSftp {
    fn handle(&mut self, id: u32, path: String) -> Handle {
        let handle = format!("h{}", self.handles.len());
        self.handles.insert(handle.clone(), (path, false));
        Handle { id, handle }
    }

    fn path(&self, handle: &str) -> Result<String, StatusCode> {
        self.handles.get(handle).map(|h| h.0.clone()).ok_or(StatusCode::Failure)
    }
}

impl russh_sftp::server::Handler for MemSftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }

    async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, StatusCode> {
        let mut v = Version::new();
        if self.posix_rename {
            v.extensions.insert("posix-rename@openssh.com".into(), "1".into());
        }
        Ok(v)
    }

    async fn open(&mut self, id: u32, path: String, flags: OpenFlags, attrs: FileAttributes) -> Result<Handle, StatusCode> {
        let mut fs = self.fs.lock().unwrap();
        let path = match fs.0.get(&path) {
            Some(Node::Symlink(_)) => fs.resolve(&path)?,
            _ => path,
        };
        if fs.0.get(parent(&path)) != Some(&Node::Dir) {
            return Err(StatusCode::NoSuchFile);
        }
        match fs.0.get_mut(&path) {
            Some(Node::File(..)) if flags.contains(OpenFlags::EXCLUDE) => return Err(StatusCode::Failure),
            Some(Node::File(data, _)) if flags.contains(OpenFlags::TRUNCATE) => data.clear(),
            Some(Node::File(..)) => {}
            None if flags.contains(OpenFlags::CREATE) => drop(fs.0.insert(path.clone(), Node::File(Vec::new(), attrs.permissions.unwrap_or(0o644)))),
            _ => return Err(StatusCode::NoSuchFile),
        }
        drop(fs);
        Ok(self.handle(id, path))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
        self.handles.remove(&handle);
        Ok(ok(id))
    }

    async fn read(&mut self, id: u32, handle: String, offset: u64, len: u32) -> Result<Data, StatusCode> {
        let fs = self.fs.lock().unwrap();
        let data = fs.get(&self.path(&handle)?).ok_or(StatusCode::NoSuchFile)?;
        let start = (offset as usize).min(data.len());
        match &data[start..(start + len as usize).min(data.len())] {
            [] => Err(StatusCode::Eof),
            chunk => Ok(Data { id, data: chunk.to_vec() }),
        }
    }

    async fn write(&mut self, id: u32, handle: String, offset: u64, bytes: Vec<u8>) -> Result<Status, StatusCode> {
        let path = self.path(&handle)?;
        let mut fs = self.fs.lock().unwrap();
        let Some(Node::File(data, _)) = fs.0.get_mut(&path) else { return Err(StatusCode::NoSuchFile) };
        let end = offset as usize + bytes.len();
        data.resize(data.len().max(end), 0);
        data[offset as usize..end].copy_from_slice(&bytes);
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        Ok(Attrs { id, attrs: self.fs.lock().unwrap().attrs(&path)? })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        let fs = self.fs.lock().unwrap();
        Ok(Attrs { id, attrs: fs.attrs(&fs.resolve(&path)?)? })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        Ok(Attrs { id, attrs: self.fs.lock().unwrap().attrs(&self.path(&handle)?)? })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        let real = self.fs.lock().unwrap().resolve(&path)?;
        Ok(self.handle(id, real))
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        let (dir, done) = self.handles.get_mut(&handle).ok_or(StatusCode::Failure)?;
        if std::mem::replace(done, true) {
            return Err(StatusCode::Eof);
        }
        let fs = self.fs.lock().unwrap();
        let entries = fs.0.keys().filter(|p| p.as_str() != "/" && parent(p) == dir.as_str());
        let files = entries.map(|p| File { filename: p.rsplit('/').next().unwrap().into(), longname: String::new(), attrs: fs.attrs(p).unwrap() });
        Ok(Name { id, files: files.collect() })
    }

    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.fs.lock().unwrap().0.remove(&path).map(|_| ok(id)).ok_or(StatusCode::NoSuchFile)
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        Ok(Name { id, files: vec![File::dummy(self.fs.lock().unwrap().resolve(&path)?)] })
    }

    /// SFTPv3 / OpenSSH: a plain rename refuses to overwrite.
    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, StatusCode> {
        let mut fs = self.fs.lock().unwrap();
        if fs.0.contains_key(&to) {
            return Err(StatusCode::Failure);
        }
        let node = fs.0.remove(&from).ok_or(StatusCode::NoSuchFile)?;
        fs.0.insert(to, node);
        Ok(ok(id))
    }

    async fn extended(&mut self, id: u32, request: String, data: Vec<u8>) -> Result<Packet, StatusCode> {
        if request != "posix-rename@openssh.com" || !self.posix_rename {
            return Err(StatusCode::OpUnsupported);
        }
        let names: HardlinkExtension = russh_sftp::de::from_bytes(&mut bytes::Bytes::from(data)).map_err(|_| StatusCode::BadMessage)?;
        let mut fs = self.fs.lock().unwrap();
        let node = fs.0.remove(&names.oldpath).ok_or(StatusCode::NoSuchFile)?;
        fs.0.insert(names.newpath, node);
        Ok(Packet::Status(ok(id)))
    }
}

/// What the server saw, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    Exec(String),
    Signal(String),
    Forward(String, u32),
    Pty(String, u32, u32),
    Resize(u32, u32),
}

#[derive(Default)]
pub struct Log {
    pub connections: AtomicUsize,
    pub seen: Mutex<Vec<Seen>>,
    pub changed: tokio::sync::Notify,
}

impl Log {
    fn push(&self, seen: Seen) {
        self.seen.lock().unwrap().push(seen);
        self.changed.notify_waiters();
    }

    pub fn commands(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().filter_map(|s| if let Seen::Exec(c) = s { Some(c.clone()) } else { None }).collect()
    }

    pub fn has(&self, seen: &Seen) -> bool {
        self.seen.lock().unwrap().contains(seen)
    }
}

#[derive(Clone)]
struct Shared {
    authorized: PublicKey,
    fs: Arc<Mutex<MemFs>>,
    log: Arc<Log>,
    posix_rename: bool,
}

struct Conn(Shared, HashMap<ChannelId, Channel<Msg>>, HashMap<ChannelId, Vec<u8>>);

impl russh::server::Handler for Conn {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, user: &str, key: VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        Ok(if user == USER && key.public_key.key_data() == self.0.authorized.key_data() { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        self.1.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _: &str,
        _: u32,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.0.log.push(Seen::Forward(host.into(), port));
        let mut tcp = tokio::net::TcpStream::connect((host, port as u16)).await?;
        reply.accept().await;
        tokio::spawn(async move { tokio::io::copy_bidirectional(&mut channel.into_stream(), &mut tcp).await });
        Ok(())
    }

    /// `echo X` prints X, `fail` exits 3 with `boom`, `big N` prints N bytes, `hang` never ends.
    async fn exec_request(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        self.0.log.push(Seen::Exec(command.clone()));
        session.channel_success(channel)?;
        let (out, err, status): (Vec<u8>, &[u8], u32) = match command.split_once(' ').unwrap_or((&command, "")) {
            ("hang", _) => return Ok(()),
            ("echo", rest) => (format!("{rest}\n").into(), b"", 0),
            ("fail", _) => (Vec::new(), b"boom\n", 3),
            ("big", n) => (vec![b'x'; n.parse().unwrap()], b"", 0),
            _ => (format!("ran: {command}\n").into(), b"", 0),
        };
        session.data(channel, out)?;
        session.extended_data(channel, 1, err.to_vec())?;
        session.exit_status_request(channel, status)?;
        session.close(channel)
    }

    async fn pty_request(&mut self, ch: ChannelId, term: &str, c: u32, r: u32, _: u32, _: u32, _: &[(russh::Pty, u32)], s: &mut Session) -> Result<(), Self::Error> {
        self.0.log.push(Seen::Pty(term.into(), c, r));
        s.channel_success(ch)
    }

    /// A line shell: echoes input, answers `you said: <line>`, `exit` ends it with status 7.
    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<(), Self::Error> {
        self.2.insert(channel, Vec::new());
        session.channel_success(channel)?;
        session.data(channel, b"welcome\r\n$ ".to_vec())
    }

    async fn window_change_request(&mut self, _: ChannelId, c: u32, r: u32, _: u32, _: u32, _: &mut Session) -> Result<(), Self::Error> {
        self.0.log.push(Seen::Resize(c, r));
        Ok(())
    }

    async fn data(&mut self, channel: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        let Some(line) = self.2.get_mut(&channel) else { return Ok(()) };
        session.data(channel, data.to_vec())?;
        line.extend(data.iter().filter(|b| **b != b'\n'));
        let Some(end) = line.iter().position(|b| *b == b'\r') else { return Ok(()) };
        let typed = String::from_utf8_lossy(&line.drain(..=end).collect::<Vec<_>>()[..end]).into_owned();
        if typed == "exit" {
            self.2.remove(&channel);
            session.exit_status_request(channel, 7)?;
            return session.close(channel);
        }
        session.data(channel, format!("\r\nyou said: {typed}\r\n$ ").into_bytes())
    }

    async fn signal(&mut self, _: ChannelId, signal: Sig, _: &mut Session) -> Result<(), Self::Error> {
        self.0.log.push(Seen::Signal(format!("{signal:?}")));
        Ok(())
    }

    async fn subsystem_request(&mut self, id: ChannelId, name: &str, session: &mut Session) -> Result<(), Self::Error> {
        let Some(channel) = self.1.remove(&id).filter(|_| name == "sftp") else { return session.channel_failure(id) };
        session.channel_success(id)?;
        let sftp = MemSftp { fs: self.0.fs.clone(), handles: HashMap::new(), posix_rename: self.0.posix_rename };
        russh_sftp::server::run(channel.into_stream(), sftp).await;
        Ok(())
    }
}

/// A running in-process SSH server.
pub struct TestServer {
    pub port: u16,
    pub host_key: PrivateKey,
    pub fs: Arc<Mutex<MemFs>>,
    pub log: Arc<Log>,
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
        let zero = Duration::ZERO;
        let config = russh::server::Config { keys: vec![host_key.clone()], auth_rejection_time: zero, auth_rejection_time_initial: Some(zero), ..Default::default() };
        let (config, listener) = (Arc::new(config), tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
        let fs = MemFs(BTreeMap::from([("/".into(), Node::Dir), ("/srv".into(), Node::Dir)]));
        let shared = Shared { authorized: authorized.clone(), fs: Arc::new(Mutex::new(fs)), log: Arc::default(), posix_rename };
        let (port, fs, log) = (listener.local_addr().unwrap().port(), shared.fs.clone(), shared.log.clone());
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                shared.log.connections.fetch_add(1, Ordering::SeqCst);
                let (config, conn) = (config.clone(), Conn(shared.clone(), HashMap::new(), HashMap::new()));
                tokio::spawn(async move { russh::server::run_stream(config, tcp, conn).await?.await });
            }
        });
        Self { port, host_key, fs, log, task }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The `known_hosts` line for this server's real key.
    pub fn known_hosts_line(&self) -> String {
        format!("[127.0.0.1]:{} {}\n", self.port, self.host_key.public_key().to_openssh().unwrap())
    }

    pub fn connections(&self) -> usize {
        self.log.connections.load(Ordering::SeqCst)
    }
}

/// A fake ssh-agent on a temp unix socket holding one key.
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
    #[cfg(unix)]
    pub async fn start(dir: &Path) -> Self {
        let (socket, key) = (dir.join(format!("agent-{}.sock", rand::random::<u32>())), random_key());
        let stream = tokio_stream::wrappers::UnixListenerStream::new(tokio::net::UnixListener::bind(&socket).unwrap());
        let task = tokio::spawn(async move { drop(russh::keys::agent::server::serve(stream, ()).await) });
        AgentClient::connect_uds(&socket).await.unwrap().add_identity(&key, &[]).await.unwrap();
        Self { socket, key, task }
    }

    /// Windows agents listen on a named pipe; `socket` holds its name.
    #[cfg(windows)]
    pub async fn start(_dir: &Path) -> Self {
        use tokio::net::windows::named_pipe::ServerOptions;
        let (name, key) = (format!(r"\\.\pipe\loa-test-agent-{}", rand::random::<u32>()), random_key());
        let mut server = ServerOptions::new().first_pipe_instance(true).create(&name).unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        let pipe = name.clone();
        let accept = tokio::spawn(async move {
            while server.connect().await.is_ok() {
                let Ok(next) = ServerOptions::new().create(&pipe) else { return };
                if sender.send(Ok::<_, std::io::Error>(std::mem::replace(&mut server, next))).await.is_err() {
                    return;
                }
            }
        });
        let connections = tokio_stream::wrappers::ReceiverStream::new(receiver);
        let task = tokio::spawn(async move {
            let _ = russh::keys::agent::server::serve(connections, ()).await;
            accept.abort();
        });
        AgentClient::connect_named_pipe(&name).await.unwrap().add_identity(&key, &[]).await.unwrap();
        Self { socket: PathBuf::from(name), key, task }
    }

    pub fn public(&self) -> PublicKey {
        self.key.public_key().clone()
    }
}

/// Records host key prompts and answers them all alike.
pub struct Decider(pub bool, pub Mutex<Vec<HostKeyPrompt>>);

#[async_trait]
impl HostKeyDecider for Decider {
    async fn decide(&self, prompt: HostKeyPrompt) -> bool {
        self.1.lock().unwrap().push(prompt);
        self.0
    }
}

/// Answers approvals from a script (then denies) and records the requests.
pub struct Scripted(pub Mutex<Vec<Approval>>, pub Mutex<Vec<ApprovalRequest>>);

#[async_trait]
impl Approver for Scripted {
    async fn approve(&self, request: ApprovalRequest) -> Approval {
        self.1.lock().unwrap().push(request);
        let mut answers = self.0.lock().unwrap();
        if answers.is_empty() { Approval::Deny { reason: None } } else { answers.remove(0) }
    }
}

/// How [`Setup::build`] wires the service against one server whose host is `box`.
pub struct Setup {
    pub limits: HostLimits,
    pub approvals: Vec<Approval>,
    /// `known_hosts` text; `None` trusts the server's key.
    pub known: Option<String>,
    pub accept_keys: bool,
    pub posix_rename: bool,
    /// Whether the server accepts the agent's key, and whether the app has an agent at all.
    pub authorized: bool,
    pub agent: bool,
}

pub fn setup(limits: HostLimits) -> Setup {
    Setup { limits, approvals: Vec::new(), known: None, accept_keys: false, posix_rename: true, authorized: true, agent: true }
}

/// A wired service and what it talks to.
pub struct Rig {
    pub dir: tempfile::TempDir,
    pub agent: FakeAgent,
    pub server: TestServer,
    pub known_hosts: PathBuf,
    pub hosts: Arc<HostRegistry>,
    pub audit: Arc<MemoryAudit>,
    pub decider: Arc<Decider>,
    pub approver: Arc<Scripted>,
    pub service: Arc<RemoteService>,
}

/// A direct hop to `127.0.0.1:port` as [`USER`].
pub fn endpoint(port: u16) -> Endpoint {
    Endpoint { alias: "127.0.0.1".into(), hostname: "127.0.0.1".into(), port, user: USER.into(), agent: AgentSocket::Default }
}

impl Setup {
    pub async fn build(self) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let agent = FakeAgent::start(dir.path()).await;
        let server = TestServer::start_with(&if self.authorized { agent.public() } else { random_key().public_key().clone() }, self.posix_rename).await;
        let known_hosts = dir.path().join("known_hosts");
        std::fs::write(&known_hosts, self.known.unwrap_or_else(|| server.known_hosts_line())).unwrap();
        let (hosts, audit) = (Arc::new(HostRegistry::new()), Arc::new(MemoryAudit::default()));
        hosts.insert(Host { name: "box".into(), target: endpoint(server.port), jumps: Vec::new(), limits: self.limits });
        let decider = Arc::new(Decider(self.accept_keys, Mutex::default()));
        let approver = Arc::new(Scripted(Mutex::new(self.approvals), Mutex::default()));
        let connector = ConnectorConfig::with_agent(self.agent.then(|| agent.socket.clone()));
        let config = RemoteServiceConfig { known_hosts: vec![known_hosts.clone()], known_hosts_write: known_hosts.clone(), connector };
        let service = Arc::new(RemoteService::new(hosts.clone(), audit.clone(), decider.clone(), approver.clone(), config));
        Rig { dir, agent, server, known_hosts, hosts, audit, decider, approver, service }
    }
}
