use loa_p2p::transport::{Connection, ProtocolHandler, Quic, RelayConfig, Router, Transport};
use loa_p2p::wire::{self, ControlFrame, Control};
use std::sync::Arc;

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

#[tokio::main]
async fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "dial" {
        // dial <hex key> <addr>
        let key: loa_p2p::NodeId = std::env::args().nth(2).unwrap().parse().unwrap();
        let addr: std::net::SocketAddr = std::env::args().nth(3).unwrap().parse().unwrap();
        let q = Quic::bind([2; 32], "127.0.0.1:0".parse().unwrap(), RelayConfig::Off, Router::default()).await.unwrap();
        let conn = q.dial(&key, &[addr], wire::CONTROL).await.unwrap();
        let mut s = conn.open_stream().await.unwrap();
        let hello = ControlFrame::from(Control::Hello(wire::Hello { app_version: "0.1.0".into(), protocols: vec![], name: "rust".into() }));
        let answer: ControlFrame = wire::call(&mut s, &hello).await.unwrap();
        println!("RUST DIAL OK {answer:?}");
        conn.close(0, b"done");
        return;
    }
    let router = Router::default().handler(wire::CONTROL, Arc::new(Echo));
    let q = Quic::bind([1; 32], "127.0.0.1:0".parse().unwrap(), RelayConfig::Off, router).await.unwrap();
    println!("ADDR {}", q.local_addrs()[0]);
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
}
