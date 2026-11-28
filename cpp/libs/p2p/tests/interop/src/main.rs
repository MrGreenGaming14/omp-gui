use futures::future::poll_fn;
use loa_p2p::transport::{Connection, ProtocolHandler, Quic, RelayConfig, Router, Tcp, Transport};
use loa_p2p::wire::{self, Control, ControlFrame};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio_util::compat::{FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};

struct Echo;
#[async_trait::async_trait]
impl ProtocolHandler for Echo {
    async fn handle(&self, conn: Connection) {
        let mut s = conn.accept_stream().await.unwrap();
        let hello: ControlFrame = wire::recv(&mut s.recv).await.unwrap().unwrap();
        eprintln!("rust got {hello:?}");
        let Some(Control::Hello(mut h)) = hello.body else { panic!() };
        h.name = "rust".into();
        wire::send(&mut s.send, &ControlFrame::from(Control::Hello(h))).await.unwrap();
        tokio::io::AsyncWriteExt::shutdown(&mut s.send).await.unwrap();
        conn.closed().await;
    }
}

/// Records every byte written through it.
struct Tee<T> {
    inner: T,
    log: Arc<Mutex<Vec<u8>>>,
}

impl<T: AsyncRead + Unpin> AsyncRead for Tee<T> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for Tee<T> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        let n = std::task::ready!(Pin::new(&mut self.inner).poll_write(cx, buf))?;
        self.log.lock().unwrap().extend_from_slice(&buf[..n]);
        Poll::Ready(Ok(n))
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> { Pin::new(&mut self.inner).poll_flush(cx) }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> { Pin::new(&mut self.inner).poll_shutdown(cx) }
}

/// A Rust yamux client opens one stream, sends the stream prefix and "ping", half-closes,
/// reads the server's "pong", then closes the connection; writes what the client sent.
async fn yamux_capture(out: &str) {
    let (a, b) = tokio::io::duplex(1 << 20);
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut client = yamux::Connection::new(Tee { inner: a, log: log.clone() }.compat(), yamux::Config::default(), yamux::Mode::Client);
    let mut server = yamux::Connection::new(b.compat(), yamux::Config::default(), yamux::Mode::Server);
    let server_task = tokio::spawn(async move {
        while let Some(Ok(stream)) = poll_fn(|cx| server.poll_next_inbound(cx)).await {
            tokio::spawn(async move {
                let mut s = stream.compat();
                let mut buf = Vec::new();
                s.read_to_end(&mut buf).await.unwrap();
                s.write_all(b"pong").await.unwrap();
                s.shutdown().await.unwrap();
            });
        }
    });
    let stream = poll_fn(|cx| client.poll_new_outbound(cx)).await.unwrap();
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let mut s = stream.compat();
        s.write_all(&[0]).await.unwrap();
        s.write_all(b"ping").await.unwrap();
        s.shutdown().await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"pong");
        let _ = done_tx.send(());
    });
    loop {
        tokio::select! {
            _ = &mut done_rx => break,
            next = poll_fn(|cx| client.poll_next_inbound(cx)) => if next.is_none() { break },
        }
    }
    poll_fn(|cx| client.poll_close(cx)).await.unwrap();
    server_task.abort();
    std::fs::write(out, &*log.lock().unwrap()).unwrap();
}

/// A Rust yamux server reads the client bytes in `file`; prints each stream's payload.
async fn yamux_decode(file: &str) {
    let bytes = std::fs::read(file).unwrap();
    let (mut feed, sock) = tokio::io::duplex(1 << 20);
    feed.write_all(&bytes).await.unwrap();
    feed.shutdown().await.unwrap();
    let mut server = yamux::Connection::new(sock.compat(), yamux::Config::default(), yamux::Mode::Server);
    let mut readers = Vec::new();
    let end = loop {
        match poll_fn(|cx| server.poll_next_inbound(cx)).await {
            Some(Ok(stream)) => readers.push(tokio::spawn(async move {
                let id = stream.id();
                let mut s = stream.compat();
                let mut buf = Vec::new();
                let _ = s.read_to_end(&mut buf).await;
                (id, buf)
            })),
            Some(Err(e)) => break format!("error {e}"),
            None => break "closed".to_owned(),
        }
    };
    for r in readers {
        let (id, buf) = r.await.unwrap();
        let hex: String = buf.iter().map(|b| format!("{b:02x}")).collect();
        println!("STREAM {id} {hex}");
    }
    println!("END {end}");
    drop(feed);
}

async fn serve(transport: Arc<dyn Transport>) {
    println!("ADDR {}", transport.local_addrs()[0]);
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
}

async fn dial(transport: Arc<dyn Transport>) {
    // dial <hex key> <addr>
    let key: loa_p2p::NodeId = std::env::args().nth(2).unwrap().parse().unwrap();
    let addr: std::net::SocketAddr = std::env::args().nth(3).unwrap().parse().unwrap();
    let conn = transport.dial(&key, &[addr], wire::CONTROL).await.unwrap();
    let mut s = conn.open_stream().await.unwrap();
    let hello = ControlFrame::from(Control::Hello(wire::Hello { app_version: "0.1.0".into(), protocols: vec![], name: "rust".into() }));
    let answer: ControlFrame = wire::call(&mut s, &hello).await.unwrap();
    println!("RUST DIAL OK {answer:?}");
    conn.close(0, b"done");
}

#[tokio::main]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let loopback = "127.0.0.1:0".parse().unwrap();
    let echo = || Router::default().handler(wire::CONTROL, Arc::new(Echo));
    match mode.as_str() {
        "dial" => dial(Arc::new(Quic::bind([2; 32], loopback, RelayConfig::Off, Router::default()).await.unwrap())).await,
        "tcp-dial" => dial(Arc::new(Tcp::bind([2; 32], loopback, Router::default()).await.unwrap())).await,
        "tcp-serve" => serve(Arc::new(Tcp::bind([1; 32], loopback, echo()).await.unwrap())).await,
        "yamux-capture" => yamux_capture(&std::env::args().nth(2).unwrap()).await,
        "yamux-decode" => yamux_decode(&std::env::args().nth(2).unwrap()).await,
        _ => serve(Arc::new(Quic::bind([1; 32], loopback, RelayConfig::Off, echo()).await.unwrap())).await,
    }
}
