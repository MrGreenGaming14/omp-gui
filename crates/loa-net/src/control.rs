//! `omp/control/1`: version negotiation, presence and opaque grant messages.
//! The transport authenticates the node key; these messages never carry keys or
//! interpret/authorize Biscuit tokens. Grant verification belongs to grants.

use crate::conn::{RecvHalf, SendHalf, Stream};
use crate::error::{NetError, Result};
use crate::peers::{DiscoverySource, PeerStore};
use crate::NodeId;
use prost::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Maximum encoded control message (exclusive of the four-byte length).
pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

#[derive(Clone, PartialEq, Message)]
pub struct ProtocolVersion {
    /// Name without version, e.g. `omp/session`.
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(uint32, tag = "2")]
    pub version: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Hello {
    #[prost(string, tag = "1")]
    pub app_version: String,
    #[prost(message, repeated, tag = "2")]
    pub protocols: Vec<ProtocolVersion>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Presence {
    #[prost(bool, tag = "1")]
    pub online: bool,
    /// Unix epoch milliseconds of the last observation of this node.
    #[prost(int64, optional, tag = "2")]
    pub last_seen_ms: Option<i64>,
}

#[derive(Clone, PartialEq, Message)]
pub struct GrantDelivery {
    /// Serialized Biscuit, opaque to this layer.
    #[prost(bytes = "vec", tag = "1")]
    pub token: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct GrantRenewal {
    #[prost(bytes = "vec", tag = "1")]
    pub grant_id: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct GrantRenewalResult {
    /// Fresh opaque token if renewal succeeded; absent if refused.
    #[prost(bytes = "vec", optional, tag = "1")]
    pub token: Option<Vec<u8>>,
    #[prost(string, tag = "2")]
    pub refusal: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct GrantRevocation {
    #[prost(bytes = "vec", tag = "1")]
    pub grant_id: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ControlFrame {
    #[prost(oneof = "control_frame::Kind", tags = "1, 2, 3, 4, 5, 6")]
    pub kind: Option<control_frame::Kind>,
}

pub mod control_frame {
    use super::*;
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Kind {
        #[prost(message, tag = "1")]
        Hello(Hello),
        #[prost(message, tag = "2")]
        Presence(Presence),
        #[prost(message, tag = "3")]
        GrantDelivery(GrantDelivery),
        #[prost(message, tag = "4")]
        GrantRenewal(GrantRenewal),
        #[prost(message, tag = "5")]
        GrantRenewalResult(GrantRenewalResult),
        #[prost(message, tag = "6")]
        GrantRevocation(GrantRevocation),
    }
}

/// Sends a length-prefixed protobuf frame. Never sends a partial oversize frame.
pub async fn send_frame(send: &mut SendHalf, frame: &ControlFrame) -> Result<()> {
    if frame.kind.is_none() {
        return Err(NetError::Protocol("empty control frame".into()));
    }
    let len = frame.encoded_len();
    if len > MAX_FRAME_SIZE {
        return Err(NetError::Protocol(format!("control frame exceeds {MAX_FRAME_SIZE} bytes")));
    }
    let mut encoded = Vec::with_capacity(len);
    frame.encode(&mut encoded).map_err(|e| NetError::Protocol(e.to_string()))?;
    send.write_all(&(len as u32).to_be_bytes()).await?;
    send.write_all(&encoded).await?;
    send.flush().await?;
    Ok(())
}

/// Returns `None` only on a clean stream EOF before the next frame. A
/// truncated header/payload or an oversized advertised size is an error.
pub async fn recv_frame(recv: &mut RecvHalf) -> Result<Option<ControlFrame>> {
    let mut header = [0u8; 4];
    if recv.read(&mut header[..1]).await? == 0 {
        return Ok(None);
    }
    recv.read_exact(&mut header[1..]).await?;
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME_SIZE {
        return Err(NetError::Protocol(format!("invalid control frame size: {len}")));
    }
    let mut data = vec![0; len];
    recv.read_exact(&mut data).await?;
    let frame = ControlFrame::decode(data.as_slice())?;
    if frame.kind.is_none() {
        return Err(NetError::Protocol("empty control frame".into()));
    }
    Ok(Some(frame))
}

/// Both sides send first, then read. The caller supplies the remote key proven
/// by the connection handshake, not an untrusted key from a message.
pub async fn exchange_hello(stream: &mut Stream, local: Hello, remote: NodeId, peers: &PeerStore) -> Result<Hello> {
    send_frame(&mut stream.send, &ControlFrame { kind: Some(control_frame::Kind::Hello(local)) }).await?;
    let received = recv_frame(&mut stream.recv).await?
        .ok_or_else(|| NetError::Protocol("peer closed before hello".into()))?;
    let control_frame::Kind::Hello(hello) = received.kind.unwrap() else {
        return Err(NetError::Protocol("first control frame must be hello".into()));
    };
    if hello.app_version.is_empty() || hello.protocols.iter().any(|p| p.name.is_empty() || p.version == 0) {
        return Err(NetError::Protocol("invalid peer hello".into()));
    }
    if peers.lookup(&remote)?.is_none() {
        peers.add_peer(&remote.to_string(), None, &[], DiscoverySource::Manual)?;
    }
    peers.update_app_version(&remote, &hello.app_version)?;
    Ok(hello)
}

/// Apply a peer's presence message to the store; the authenticated remote
/// node, not an identifier in the message, selects the row to update.
pub fn record_presence(peers: &PeerStore, remote: &NodeId, presence: &Presence) -> Result<bool> {
    if presence.online && presence.last_seen_ms.is_none() {
        return Err(NetError::Protocol("online presence requires last_seen_ms".into()));
    }
    peers.update_presence(remote, presence.online, presence.last_seen_ms.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, split};

    fn key(seed: u8) -> NodeId {
        iroh::SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn stream_pair() -> (Stream, Stream) {
        let (a, b) = duplex(64 * 1024);
        let (ar, aw) = split(a);
        let (br, bw) = split(b);
        (Stream { send: Box::new(aw), recv: Box::new(ar) }, Stream { send: Box::new(bw), recv: Box::new(br) })
    }

    #[tokio::test]
    async fn two_nodes_exchange_hello_and_record_authenticated_versions() {
        let (mut a, mut b) = stream_pair();
        let a_store = PeerStore::in_memory().unwrap();
        let b_store = PeerStore::in_memory().unwrap();
        let a_key = key(1);
        let b_key = key(2);
        let a_hello = Hello { app_version: "1.0".into(), protocols: vec![ProtocolVersion { name: "omp/control".into(), version: 1 }] };
        let b_hello = Hello { app_version: "2.3".into(), protocols: vec![ProtocolVersion { name: "omp/session".into(), version: 2 }] };
        let (a_result, b_result) = tokio::join!(
            exchange_hello(&mut a, a_hello.clone(), b_key, &a_store),
            exchange_hello(&mut b, b_hello.clone(), a_key, &b_store),
        );
        assert_eq!(a_result.unwrap(), b_hello);
        assert_eq!(b_result.unwrap(), a_hello);
        assert_eq!(a_store.lookup(&b_key).unwrap().unwrap().app_version.as_deref(), Some("2.3"));
        assert_eq!(b_store.lookup(&a_key).unwrap().unwrap().app_version.as_deref(), Some("1.0"));
        record_presence(&a_store, &b_key, &Presence { online: true, last_seen_ms: Some(123) }).unwrap();
        assert_eq!(a_store.lookup(&b_key).unwrap().unwrap().last_seen_ms, Some(123));
    }

    #[tokio::test]
    async fn grant_messages_round_trip_and_frame_bounds_are_enforced() {
        let (mut a, mut b) = stream_pair();
        let messages = [
            control_frame::Kind::GrantDelivery(GrantDelivery { token: vec![1, 2, 3] }),
            control_frame::Kind::GrantRenewal(GrantRenewal { grant_id: vec![4, 5] }),
            control_frame::Kind::GrantRenewalResult(GrantRenewalResult { token: None, refusal: "revoked".into() }),
            control_frame::Kind::GrantRevocation(GrantRevocation { grant_id: vec![4, 5] }),
        ];
        for kind in messages {
            let expected = ControlFrame { kind: Some(kind) };
            send_frame(&mut a.send, &expected).await.unwrap();
            assert_eq!(recv_frame(&mut b.recv).await.unwrap(), Some(expected));
        }
        assert!(send_frame(&mut a.send, &ControlFrame { kind: Some(control_frame::Kind::GrantDelivery(GrantDelivery { token: vec![0; MAX_FRAME_SIZE] })) }).await.is_err());
        a.send.write_all(&((MAX_FRAME_SIZE + 1) as u32).to_be_bytes()).await.unwrap();
        assert!(matches!(recv_frame(&mut b.recv).await, Err(NetError::Protocol(_))));
    }
}
