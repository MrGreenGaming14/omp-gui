//! The TCP adapter: mutually authenticated TLS 1.3 with the node's Ed25519
//! key as a self-signed certificate, and yamux streams.

use super::{Connection, PeerConnection, Router, Stream, Transport, TransportKind, check_alpn};
use crate::{NodeError, NodeId, Result, net};
use async_trait::async_trait;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use futures::future::poll_fn;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::ParsedCertificate;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, DistinguishedName, ServerConfig, SignatureScheme};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc, oneshot, watch};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
use yamux::{Config, Mode};

type Tls<T> = std::result::Result<T, rustls::Error>;
const QUEUE: usize = 64;
/// First byte of every yamux stream: an application stream (1 was datagrams).
const STREAM_FRAME: u8 = 0;
const ED25519_SPKI_PREFIX: &[u8] = &[0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];

/// A TCP listener and dialer keyed by the same seed as the QUIC endpoint.
pub struct Tcp {
    addr: SocketAddr,
    listener: tokio::task::JoinHandle<()>,
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
}

impl Drop for Tcp {
    fn drop(&mut self) { self.listener.abort(); }
}

impl Tcp {
    pub async fn bind(seed: [u8; 32], bind: SocketAddr, router: Router) -> Result<Self> {
        let der = ed25519_dalek::SigningKey::from_bytes(&seed).to_pkcs8_der().map_err(net)?;
        let key = PrivatePkcs8KeyDer::from(der.as_bytes().to_vec());
        let pair = KeyPair::from_der_and_sign_algo(&PrivateKeyDer::Pkcs8(key.clone_key()), &PKCS_ED25519).map_err(net)?;
        let cert = CertificateDer::from(CertificateParams::new(vec!["localhost".into()]).map_err(net)?.self_signed(&pair).map_err(net)?.der().to_vec());
        let mut server = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(net)?
            .with_client_cert_verifier(Arc::new(NodeVerifier(None)))
            .with_single_cert(vec![cert.clone()], PrivateKeyDer::Pkcs8(key.clone_key()))
            .map_err(net)?;
        server.alpn_protocols = router.alpns();
        let acceptor = TlsAcceptor::from(Arc::new(server));
        let listener = TcpListener::bind(bind).await?;
        let addr = listener.local_addr()?;
        let listener = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let (acceptor, router) = (acceptor.clone(), router.clone());
                tokio::spawn(async move {
                    if let Err(err) = accept(socket, acceptor, router).await {
                        tracing::debug!(%err, "TCP peer handshake ended");
                    }
                });
            }
        });
        Ok(Self { addr, listener, cert, key })
    }

    async fn connect(&self, expected: Option<NodeId>, addr: SocketAddr, alpn: &[u8]) -> Result<Connection> {
        let mut config = ClientConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(net)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NodeVerifier(expected)))
            .with_client_auth_cert(vec![self.cert.clone()], PrivateKeyDer::Pkcs8(self.key.clone_key()))
            .map_err(net)?;
        config.alpn_protocols = vec![alpn.to_vec()];
        let failed = |e: &dyn std::fmt::Display| NodeError::InvalidAddress(addr.to_string(), e.to_string());
        let socket = TcpStream::connect(addr).await.map_err(|e| failed(&e))?;
        let tls = TlsConnector::from(Arc::new(config))
            .connect(ServerName::try_from("localhost").expect("static DNS name"), socket)
            .await
            .map_err(|e| failed(&e))?;
        let peer = peer_of(tls.get_ref().1.peer_certificates())?;
        if let Some(expected) = expected.filter(|e| *e != peer) {
            return Err(NodeError::WrongPeer(expected, peer));
        }
        check_alpn(peer, alpn, tls.get_ref().1.alpn_protocol())?;
        Ok(start(tls, peer, alpn.to_vec(), Mode::Client))
    }
}

#[async_trait]
impl Transport for Tcp {
    async fn dial(&self, peer: &NodeId, hints: &[SocketAddr], alpn: &[u8]) -> Result<Connection> {
        let mut last = NodeError::Connect(*peer, "no TCP address hints".into());
        for addr in hints {
            match self.connect(Some(*peer), *addr, alpn).await {
                Ok(conn) => return Ok(conn),
                Err(NodeError::InvalidAddress(_, reason)) => last = NodeError::Connect(*peer, reason),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    async fn dial_any(&self, addr: SocketAddr, alpn: &[u8]) -> Result<Connection> { self.connect(None, addr, alpn).await }

    fn local_addrs(&self) -> Vec<SocketAddr> { vec![self.addr] }

    async fn close(&self) { self.listener.abort(); }
}

async fn accept(socket: TcpStream, acceptor: TlsAcceptor, router: Router) -> Result<()> {
    let tls = acceptor.accept(socket).await?;
    let peer = peer_of(tls.get_ref().1.peer_certificates())?;
    let alpn = tls.get_ref().1.alpn_protocol().ok_or_else(|| NodeError::Protocol("no shared ALPN".into()))?.to_vec();
    let conn = start(tls, peer, alpn, Mode::Server);
    router.dispatch(conn.clone()).await;
    // Keep the mux alive until the peer closes, so queued answers are flushed.
    conn.closed().await;
    Ok(())
}

/// The node key of a canonical Ed25519 leaf certificate; TLS proved possession.
fn certificate_id(cert: &CertificateDer<'_>) -> Result<NodeId> {
    let parsed = ParsedCertificate::try_from(cert).map_err(net)?;
    let key: [u8; 32] = parsed.subject_public_key_info().as_ref().strip_prefix(ED25519_SPKI_PREFIX).and_then(|b| b.try_into().ok())
        .ok_or_else(|| NodeError::InvalidKey("not an Ed25519 node certificate".into()))?;
    NodeId::from_bytes(&key).map_err(|e| NodeError::InvalidKey(e.to_string()))
}

fn peer_of(certs: Option<&[CertificateDer<'_>]>) -> Result<NodeId> {
    certificate_id(certs.and_then(|c| c.first()).ok_or_else(|| NodeError::Net("the peer sent no certificate".into()))?)
}

/// The one verifier of both sides: a single Ed25519 leaf, of `expected` when set.
#[derive(Debug)]
struct NodeVerifier(Option<NodeId>);

impl NodeVerifier {
    fn check(&self, cert: &CertificateDer<'_>, chain: &[CertificateDer<'_>]) -> Tls<()> {
        let general = |text: String| rustls::Error::General(text);
        if !chain.is_empty() {
            return Err(general("node certificate must be a single leaf".into()));
        }
        let id = certificate_id(cert).map_err(|e| general(e.to_string()))?;
        if self.0.is_some_and(|expected| expected != id) {
            return Err(general("remote node key differs from requested peer".into()));
        }
        Ok(())
    }
}

/// The signature methods both verifier traits share.
macro_rules! signatures {
    () => {
        fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &DigitallySignedStruct) -> Tls<HandshakeSignatureValid> {
            Err(rustls::Error::General("TLS 1.2 disabled".into()))
        }
        fn verify_tls13_signature(&self, msg: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Tls<HandshakeSignatureValid> {
            if dss.scheme != SignatureScheme::ED25519 {
                return Err(rustls::Error::General("expected Ed25519 signature".into()));
            }
            verify_tls13_signature(msg, cert, dss, &ring::default_provider().signature_verification_algorithms)
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> { vec![SignatureScheme::ED25519] }
    };
}

impl ServerCertVerifier for NodeVerifier {
    fn verify_server_cert(&self, cert: &CertificateDer<'_>, chain: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Tls<ServerCertVerified> {
        self.check(cert, chain).map(|()| ServerCertVerified::assertion())
    }
    signatures!();
}

impl ClientCertVerifier for NodeVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] { &[] }
    fn verify_client_cert(&self, cert: &CertificateDer<'_>, chain: &[CertificateDer<'_>], _: UnixTime) -> Tls<ClientCertVerified> {
        self.check(cert, chain).map(|()| ClientCertVerified::assertion())
    }
    signatures!();
}

type Yamux = Compat<yamux::Stream>;

struct TcpConnection {
    peer: NodeId,
    alpn: Vec<u8>,
    open: mpsc::Sender<oneshot::Sender<Result<Yamux>>>,
    inbound: Mutex<mpsc::Receiver<Stream>>,
    done: watch::Sender<Option<String>>,
}

fn closed() -> NodeError { NodeError::Net("TCP connection ended".into()) }

fn split(stream: Yamux) -> Stream {
    let (recv, send) = tokio::io::split(stream);
    Stream { send: Box::new(send), recv: Box::new(recv) }
}

fn start<T: AsyncRead + AsyncWrite + Send + Unpin + 'static>(tls: T, peer: NodeId, alpn: Vec<u8>, mode: Mode) -> Connection {
    let (open, requests) = mpsc::channel(QUEUE);
    let (streams, inbound) = mpsc::channel(QUEUE);
    let (done, _) = watch::channel(None);
    let mux = yamux::Connection::new(tls.compat(), Config::default(), mode);
    tokio::spawn(drive(mux, requests, streams, done.clone()));
    Arc::new(TcpConnection { peer, alpn, open, inbound: Mutex::new(inbound), done })
}

/// Runs the mux: opens requested streams, hands out inbound ones, until either side closes.
async fn drive<T>(mut mux: yamux::Connection<T>, mut requests: mpsc::Receiver<oneshot::Sender<Result<Yamux>>>, streams: mpsc::Sender<Stream>, done: watch::Sender<Option<String>>)
where T: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin + Send + 'static {
    let mut close = done.subscribe();
    let reason = loop {
        tokio::select! {
            request = requests.recv() => {
                let Some(reply) = request else { break "connection handles dropped".to_owned() };
                let _ = reply.send(poll_fn(|cx| mux.poll_new_outbound(cx)).await.map(|s| s.compat()).map_err(net));
            }
            inbound = poll_fn(|cx| mux.poll_next_inbound(cx)) => match inbound {
                Some(Ok(stream)) => {
                    let streams = streams.clone();
                    tokio::spawn(async move {
                        let mut stream = stream.compat();
                        let mut kind = [0];
                        if stream.read_exact(&mut kind).await.is_ok() && kind[0] == STREAM_FRAME {
                            let _ = streams.send(split(stream)).await;
                        }
                    });
                }
                Some(Err(e)) => break e.to_string(),
                None => break "peer closed the connection".to_owned(),
            },
            changed = close.changed() => match (changed, close.borrow_and_update().clone()) {
                (Err(_), _) => break "connection closed".to_owned(),
                (_, Some(reason)) => break reason,
                _ => {}
            },
        }
    };
    done.send_replace(Some(reason));
}

#[async_trait]
impl PeerConnection for TcpConnection {
    fn remote(&self) -> NodeId { self.peer }
    fn alpn(&self) -> &[u8] { &self.alpn }
    fn kind(&self) -> TransportKind { TransportKind::Tcp }
    async fn open_stream(&self) -> Result<Stream> {
        let (reply, rx) = oneshot::channel();
        self.open.send(reply).await.map_err(|_| closed())?;
        let mut stream = rx.await.map_err(|_| closed())??;
        stream.write_all(&[STREAM_FRAME]).await?;
        Ok(split(stream))
    }
    async fn accept_stream(&self) -> Result<Stream> { self.inbound.lock().await.recv().await.ok_or_else(closed) }
    fn close(&self, code: u32, reason: &[u8]) { self.done.send_replace(Some(format!("{code}: {}", String::from_utf8_lossy(reason)))); }
    async fn closed(&self) -> String {
        let mut done = self.done.subscribe();
        loop {
            if let Some(reason) = done.borrow_and_update().clone() {
                return reason;
            }
            if done.changed().await.is_err() {
                return "connection closed".into();
            }
        }
    }
}
