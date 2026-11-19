//! Mutually authenticated TLS 1.3 TCP fallback, with yamux streams and bounded datagrams.
use crate::alpn::{self, Alpn};
use crate::conn::{Connection, PeerConnection, Stream, Transport};
use crate::error::{NetError, Result};
use crate::router::Router;
use crate::NodeId;
use async_trait::async_trait;
use bytes::Bytes;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use futures::future::poll_fn;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::client::danger::{ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::ParsedCertificate;
use rustls::{ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme};
use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use yamux::{Config, Mode};

const MAX_DATAGRAM: usize = 1200;
const QUEUE: usize = 64;
const STREAM_FRAME: u8 = 0;
const DATAGRAM_FRAME: u8 = 1;
const ED25519_SPKI_PREFIX: &[u8] = &[
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// TCP listener and dialer using the same Ed25519 seed as the iroh endpoint.
pub struct TcpNode {
    id: NodeId,
    listener_task: tokio::task::JoinHandle<()>,
    addr: SocketAddr,
    identity: (CertificateDer<'static>, PrivatePkcs8KeyDer<'static>),
}

impl Drop for TcpNode {
    fn drop(&mut self) { self.listener_task.abort(); }
}

impl TcpNode {
    pub async fn bind(seed: [u8; 32], bind: SocketAddr, router: Router) -> Result<Self> {
        let secret = iroh::SecretKey::from_bytes(&seed);
        let id = secret.public();
        let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
        let key_der = signing.to_pkcs8_der().map_err(|e| NetError::InvalidKey(e.to_string()))?;
        let private = PrivatePkcs8KeyDer::from(key_der.as_bytes().to_vec());
        let key = KeyPair::from_der_and_sign_algo(&PrivateKeyDer::Pkcs8(private.clone_key()), &PKCS_ED25519)
            .map_err(|e| NetError::InvalidKey(e.to_string()))?;
        let cert = CertificateParams::new(vec!["localhost".into()])
            .map_err(|e| NetError::Tls(e.to_string()))?
            .self_signed(&key).map_err(|e| NetError::Tls(e.to_string()))?;
        let cert = CertificateDer::from(cert.der().to_vec());
        let mut server = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| NetError::Tls(e.to_string()))?
            .with_client_cert_verifier(Arc::new(NodeVerifier { expected: None }))
            .with_single_cert(vec![cert.clone()], PrivateKeyDer::Pkcs8(private.clone_key()))
            .map_err(|e| NetError::Tls(e.to_string()))?;
        server.alpn_protocols = router.alpns();
        let server = Arc::new(server);
        let listener = TcpListener::bind(bind).await?;
        let addr = listener.local_addr()?;
        let acceptor = TlsAcceptor::from(server);
        let listener_task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else { break };
                let acceptor = acceptor.clone();
                let router = router.clone();
                tokio::spawn(async move {
                    if let Err(err) = accept_one(socket, acceptor, router).await {
                        tracing::debug!(%err, "TCP peer handshake ended");
                    }
                });
            }
        });
        Ok(Self { id, listener_task, addr, identity: (cert, private) })
    }

    pub fn id(&self) -> NodeId { self.id }
    pub fn local_addr(&self) -> SocketAddr { self.addr }

    /// Stops accepting connections. Open connections run until either side closes.
    pub fn close(&self) { self.listener_task.abort(); }

    pub async fn dial(&self, peer: NodeId, addr: SocketAddr, protocol: &str, versions: &[u32]) -> Result<Connection> {
        self.connect(Some(peer), addr, protocol, versions).await
            .map_err(|reason| match reason {
                Dialed::Failed(reason) => NetError::Connect { peer, reason },
                Dialed::Error(e) => e,
            })
    }

    /// Dials whatever node answers at `addr` and returns the key it proved in
    /// the TLS handshake ([`PeerConnection::remote`]). For probing addresses
    /// learned without a key (Tailscale names); the caller decides whether to
    /// trust that node.
    pub async fn dial_any(&self, addr: SocketAddr, protocol: &str, versions: &[u32]) -> Result<Connection> {
        self.connect(None, addr, protocol, versions).await
            .map_err(|reason| match reason {
                Dialed::Failed(reason) => NetError::InvalidAddress(format!("{addr}: {reason}")),
                Dialed::Error(e) => e,
            })
    }

    async fn connect(&self, expected: Option<NodeId>, addr: SocketAddr, protocol: &str, versions: &[u32]) -> Result<Connection, Dialed> {
        let offers = alpn::offer(protocol, versions);
        if offers.is_empty() { return Err(Dialed::Failed(format!("no version of {protocol} offered"))); }
        let mut config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(|e| Dialed::Error(NetError::Tls(e.to_string())))?
            .dangerous().with_custom_certificate_verifier(Arc::new(NodeVerifier { expected }))
            .with_client_auth_cert(vec![self.identity.0.clone()], PrivateKeyDer::Pkcs8(self.identity.1.clone_key()))
            .map_err(|e| Dialed::Error(NetError::Tls(e.to_string())))?;
        config.alpn_protocols = offers;
        let socket = TcpStream::connect(addr).await.map_err(|e| Dialed::Failed(e.to_string()))?;
        let tls = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost").expect("static DNS name"), socket)
            .await.map_err(|e| Dialed::Failed(e.to_string()))?;
        let cert = tls.get_ref().1.peer_certificates().and_then(|c| c.first())
            .ok_or_else(|| Dialed::Error(NetError::Tls("server sent no certificate".into())))?;
        let verified = certificate_id(cert).map_err(Dialed::Error)?;
        if let Some(peer) = expected && verified != peer {
            return Err(Dialed::Error(NetError::WrongPeer { expected: peer, actual: verified }));
        }
        let unshared = || Dialed::Error(NetError::NoSharedVersion { peer: verified, protocol: protocol.into() });
        let selected = tls.get_ref().1.alpn_protocol().ok_or_else(unshared)?;
        let label = Alpn::parse(selected).map_err(Dialed::Error)?;
        if label.protocol != protocol || !versions.contains(&label.version) {
            return Err(unshared());
        }
        Ok(start_connection(tls, verified, label, Mode::Client))
    }
}

/// Why a TCP dial failed: reaching the address, or anything after it.
enum Dialed {
    Failed(String),
    Error(NetError),
}

async fn accept_one(socket: TcpStream, acceptor: TlsAcceptor, router: Router) -> Result<()> {
    let tls = acceptor.accept(socket).await.map_err(|e| NetError::Tls(e.to_string()))?;
    let cert = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).ok_or_else(|| NetError::Tls("client sent no certificate".into()))?;
    let peer = certificate_id(cert)?;
    let selected = tls.get_ref().1.alpn_protocol().ok_or_else(|| NetError::InvalidAlpn("no shared ALPN".into()))?;
    let label = Alpn::parse(selected)?;
    let conn = start_connection(tls, peer, label, Mode::Server);
    let lifecycle = conn.clone();
    router.dispatch(conn).await;
    // A handler may finish after closing its last stream; keep the mux alive
    // until its peer closes so queued response frames can be flushed.
    lifecycle.closed().await;
    Ok(())
}

// rustls parses the X.509 structure; only the canonical Ed25519 SPKI form is accepted.
// TLS CertificateVerify then proves possession of the corresponding private key.
fn certificate_id(cert: &CertificateDer<'_>) -> Result<NodeId> {
    let parsed = ParsedCertificate::try_from(cert).map_err(|e| NetError::InvalidKey(e.to_string()))?;
    let spki = parsed.subject_public_key_info();
    let bytes = spki.as_ref();
    let key: [u8; 32] = bytes.strip_prefix(ED25519_SPKI_PREFIX)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| NetError::InvalidKey("not an Ed25519 node certificate".into()))?;
    NodeId::from_bytes(&key).map_err(|e| NetError::InvalidKey(e.to_string()))
}

#[derive(Debug)]
struct NodeVerifier { expected: Option<NodeId> }

impl NodeVerifier {
    fn verify(&self, cert: &CertificateDer<'_>, intermediates: &[CertificateDer<'_>]) -> std::result::Result<(), rustls::Error> {
        if !intermediates.is_empty() { return Err(rustls::Error::General("node certificate must be a single leaf".into())); }
        let id = certificate_id(cert).map_err(|e| rustls::Error::General(e.to_string()))?;
        if self.expected.is_some_and(|expected| expected != id) {
            return Err(rustls::Error::General("remote node key differs from requested peer".into()));
        }
        Ok(())
    }
    fn signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct)
        -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        if dss.scheme != SignatureScheme::ED25519 { return Err(rustls::Error::General("expected Ed25519 signature".into())); }
        verify_tls13_signature(message, cert, dss, &ring::default_provider().signature_verification_algorithms)
    }
}
impl ServerCertVerifier for NodeVerifier {
    fn verify_server_cert(&self, cert: &CertificateDer<'_>, chain: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime)
        -> std::result::Result<ServerCertVerified, rustls::Error> {
        self.verify(cert, chain)?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct)
        -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 disabled".into()))
    }
    fn verify_tls13_signature(&self, msg: &[u8], cert: &CertificateDer<'_>, sig: &DigitallySignedStruct)
        -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.signature(msg, cert, sig)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> { vec![SignatureScheme::ED25519] }
}
impl ClientCertVerifier for NodeVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] { &[] }
    fn verify_client_cert(&self, cert: &CertificateDer<'_>, chain: &[CertificateDer<'_>], _: UnixTime)
        -> std::result::Result<ClientCertVerified, rustls::Error> {
        self.verify(cert, chain)?;
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct)
        -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 disabled".into()))
    }
    fn verify_tls13_signature(&self, msg: &[u8], cert: &CertificateDer<'_>, sig: &DigitallySignedStruct)
        -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.signature(msg, cert, sig)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> { vec![SignatureScheme::ED25519] }
}

type YamuxStream = Compat<yamux::Stream>;
type StreamRequest = oneshot::Sender<Result<YamuxStream>>;
struct TcpConnection {
    peer: NodeId,
    label: Alpn,
    outbound: mpsc::Sender<StreamRequest>,
    inbound: tokio::sync::Mutex<mpsc::Receiver<Stream>>,
    datagram_tx: mpsc::Sender<Bytes>,
    datagram_rx: tokio::sync::Mutex<mpsc::Receiver<Bytes>>,
    done: watch::Sender<Option<String>>,
}

fn split_stream(stream: YamuxStream) -> Stream {
    let (recv, send) = tokio::io::split(stream);
    Stream { send: Box::new(send), recv: Box::new(recv) }
}

fn start_connection<T>(tls: T, peer: NodeId, label: Alpn, mode: Mode) -> Connection
where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static {
    let (outbound, requests) = mpsc::channel(QUEUE);
    let (in_tx, inbound) = mpsc::channel(QUEUE);
    let (dg_tx, datagrams) = mpsc::channel(QUEUE);
    let (dg_in_tx, datagram_rx) = mpsc::channel(QUEUE);
    let (done, _) = watch::channel(None);
    let conn = Arc::new(TcpConnection { peer, label, outbound: outbound.clone(), inbound: tokio::sync::Mutex::new(inbound), datagram_tx: dg_tx, datagram_rx: tokio::sync::Mutex::new(datagram_rx), done: done.clone() });
    tokio::spawn(async move { drive(yamux::Connection::new(tls.compat(), Config::default(), mode), requests, in_tx, dg_in_tx, done).await; });
    tokio::spawn(async move { send_datagrams(outbound, datagrams).await; });
    conn
}

async fn drive<T>(mut mux: yamux::Connection<T>, mut requests: mpsc::Receiver<StreamRequest>, streams: mpsc::Sender<Stream>, datagrams: mpsc::Sender<Bytes>, done: watch::Sender<Option<String>>)
where T: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin + Send + 'static {
    let mut close = done.subscribe();
    let reason = loop {
        tokio::select! {
            request = requests.recv() => {
                if let Some(reply) = request {
                    let opened = poll_fn(|cx| mux.poll_new_outbound(cx)).await
                        .map(|s| s.compat()).map_err(|e| NetError::Stream(e.to_string()));
                    let _ = reply.send(opened);
                } else { break "connection handles dropped".to_string(); }
            }
            inbound = poll_fn(|cx| mux.poll_next_inbound(cx)) => {
                match inbound {
                    Some(Ok(stream)) => {
                        let tx = streams.clone();
                        let datagrams = datagrams.clone();
                        tokio::spawn(async move {
                            let mut stream = stream.compat();
                            let mut kind = [0];
                            if stream.read_exact(&mut kind).await.is_err() { return; }
                            match kind[0] {
                                STREAM_FRAME => { let _ = tx.send(split_stream(stream)).await; }
                                DATAGRAM_FRAME => {
                                    let mut len = [0; 2];
                                    while stream.read_exact(&mut len).await.is_ok() {
                                        let size = usize::from(u16::from_be_bytes(len));
                                        if size > MAX_DATAGRAM { break; }
                                        let mut payload = vec![0; size];
                                        if stream.read_exact(&mut payload).await.is_err() { break; }
                                        // A slow receiver must not block other multiplexed streams.
                                        let _ = datagrams.try_send(Bytes::from(payload));
                                    }
                                }
                                _ => {}
                            }
                        });
                    }
                    Some(Err(e)) => break e.to_string(),
                    None => break "peer closed the connection".to_string(),
                }
            }
            result = close.changed() => {
                if result.is_err() { break "connection closed".to_string(); }
                if let Some(reason) = close.borrow_and_update().as_ref() { break reason.clone(); }
            }
        }
    };
    done.send_replace(Some(reason));
}

async fn send_datagrams(outbound: mpsc::Sender<StreamRequest>, mut datagrams: mpsc::Receiver<Bytes>) {
    let Some(first) = datagrams.recv().await else { return };
    let (reply, rx) = oneshot::channel();
    if outbound.send(reply).await.is_err() { return; }
    let Ok(Ok(mut stream)) = rx.await else { return };
    if stream.write_all(&[DATAGRAM_FRAME]).await.is_err() { return; }
    let mut current = Some(first);
    loop {
        let Some(data) = current.take().or_else(|| datagrams.try_recv().ok()) else {
            let Some(data) = datagrams.recv().await else { break };
            current = Some(data);
            continue;
        };
        if stream.write_all(&(data.len() as u16).to_be_bytes()).await.is_err() || stream.write_all(&data).await.is_err() { break; }
    }
}

#[async_trait]
impl PeerConnection for TcpConnection {
    fn remote(&self) -> NodeId { self.peer }
    fn alpn(&self) -> &Alpn { &self.label }
    fn transport(&self) -> Transport { Transport::Tcp }
    async fn open_stream(&self) -> Result<Stream> {
        let (reply, rx) = oneshot::channel();
        self.outbound.send(reply).await.map_err(|_| NetError::Closed("TCP connection ended".into()))?;
        let mut stream = rx.await.map_err(|_| NetError::Closed("TCP connection ended".into()))??;
        stream.write_all(&[STREAM_FRAME]).await?;
        Ok(split_stream(stream))
    }
    async fn accept_stream(&self) -> Result<Stream> {
        self.inbound.lock().await.recv().await.ok_or_else(|| NetError::Closed("TCP connection ended".into()))
    }
    fn send_datagram(&self, data: Bytes) -> Result<()> {
        if data.len() > MAX_DATAGRAM { return Err(NetError::DatagramTooLarge { size: data.len(), max: MAX_DATAGRAM }); }
        // Datagrams may be dropped under backpressure, never blocking the caller.
        match self.datagram_tx.try_send(data) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(NetError::Closed("TCP connection ended".into())),
        }
    }
    async fn recv_datagram(&self) -> Result<Bytes> {
        self.datagram_rx.lock().await.recv().await.ok_or_else(|| NetError::Closed("TCP connection ended".into()))
    }
    fn max_datagram_size(&self) -> Option<usize> { Some(MAX_DATAGRAM) }
    fn close(&self, code: u32, reason: &[u8]) {
        self.done.send_replace(Some(format!("{code}: {}", String::from_utf8_lossy(reason))));
    }
    async fn closed(&self) -> String {
        let mut done = self.done.subscribe();
        loop {
            if let Some(reason) = done.borrow_and_update().as_ref() { return reason.clone(); }
            if done.changed().await.is_err() { return "connection closed".into(); }
        }
    }
}

impl fmt::Debug for TcpConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcpConnection").field("peer", &self.peer).field("alpn", &self.label).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::router::{AllowAll, ProtocolHandler};
    use std::time::Duration;
    use tokio::sync::mpsc;

    struct Capture(mpsc::UnboundedSender<Connection>);

    #[async_trait]
    impl ProtocolHandler for Capture {
        async fn handle(&self, conn: Connection) { let _ = self.0.send(conn); }
    }

    #[tokio::test]
    async fn authenticated_tcp_multiplexes_streams_and_delivers_datagrams() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let router = Router::new(Arc::new(AllowAll))
            .handler("omp/session", 1, Arc::new(Capture(tx.clone())))
            .handler("omp/session", 3, Arc::new(Capture(tx)));
        let bind = SocketAddr::from(([127, 0, 0, 1], 0));
        let server = TcpNode::bind([41; 32], bind, router).await.unwrap();
        let client = TcpNode::bind([42; 32], bind, Router::new(Arc::new(AllowAll))).await.unwrap();
        let outgoing = client.dial(server.id(), server.local_addr(), "omp/session", &[1, 2, 3]).await.unwrap();
        let incoming = tokio::time::timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap();
        assert_eq!(outgoing.alpn().version, 3);
        assert_eq!(incoming.remote(), client.id());
        let mut first = outgoing.open_stream().await.unwrap();
        let mut second = outgoing.open_stream().await.unwrap();
        first.send.write_all(b"one").await.unwrap();
        second.send.write_all(b"two").await.unwrap();
        first.send.shutdown().await.unwrap();
        second.send.shutdown().await.unwrap();
        let mut received = Vec::new();
        for _ in 0..2 {
            let mut stream = tokio::time::timeout(Duration::from_secs(5), incoming.accept_stream()).await.unwrap().unwrap();
            let mut bytes = Vec::new();
            stream.recv.read_to_end(&mut bytes).await.unwrap();
            received.push(bytes);
        }
        received.sort();
        assert_eq!(received, [b"one".to_vec(), b"two".to_vec()]);
        outgoing.send_datagram(Bytes::from_static(b"frame")).unwrap();
        assert_eq!(tokio::time::timeout(Duration::from_secs(5), incoming.recv_datagram()).await.unwrap().unwrap(), Bytes::from_static(b"frame"));
        incoming.send_datagram(Bytes::from_static(b"input")).unwrap();
        assert_eq!(tokio::time::timeout(Duration::from_secs(5), outgoing.recv_datagram()).await.unwrap().unwrap(), Bytes::from_static(b"input"));
        assert!(matches!(outgoing.send_datagram(Bytes::from(vec![0; MAX_DATAGRAM + 1])), Err(NetError::DatagramTooLarge { .. })));
    }

    #[tokio::test]
    async fn tcp_rejects_a_different_node_key() {
        let bind = SocketAddr::from(([127, 0, 0, 1], 0));
        let (tx, _) = mpsc::unbounded_channel();
        let server = TcpNode::bind([43; 32], bind, Router::new(Arc::new(AllowAll))
            .handler("omp/control", 1, Arc::new(Capture(tx)))).await.unwrap();
        let client = TcpNode::bind([44; 32], bind, Router::new(Arc::new(AllowAll))).await.unwrap();
        let wrong = iroh::SecretKey::from_bytes(&[45; 32]).public();
        assert!(client.dial(wrong, server.local_addr(), "omp/control", &[1]).await.is_err());
    }
}
