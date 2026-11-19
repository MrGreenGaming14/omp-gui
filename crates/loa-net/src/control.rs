//! `omp/control/1`: version negotiation, presence and opaque grant messages.
//! The transport authenticates the node key; these messages never carry keys or
//! interpret/authorize Biscuit tokens. Grant verification belongs to grants.
//!
//! A control stream starts with both sides sending [`Hello`]. The dialer then
//! sends one request frame (presence, grant delivery, renewal or revocation)
//! and the listener answers it ([`Ack`] or [`GrantRenewalResult`]).

use crate::conn::{RecvHalf, SendHalf, Stream};
use crate::error::{NetError, Result};
use crate::peers::PeerStore;
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
    /// The node's display name (its host name); a hint, never an identity.
    #[prost(string, tag = "3")]
    pub name: String,
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
    /// Key of the node that minted the token's first block. Empty when it is
    /// the sender itself; a re-shared grant names the original owner.
    #[prost(bytes = "vec", tag = "2")]
    pub root: Vec<u8>,
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

/// The listener's answer to a request frame; an empty `error` means accepted.
#[derive(Clone, PartialEq, Message)]
pub struct Ack {
    #[prost(string, tag = "1")]
    pub error: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct ControlFrame {
    #[prost(oneof = "control_frame::Kind", tags = "1, 2, 3, 4, 5, 6, 7")]
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
        #[prost(message, tag = "7")]
        Ack(Ack),
    }
}

impl From<control_frame::Kind> for ControlFrame {
    fn from(kind: control_frame::Kind) -> Self {
        Self { kind: Some(kind) }
    }
}

/// Sends one length-prefixed protobuf message. Never sends a partial oversize
/// message. Shared by every protocol that frames protobuf over a stream.
pub async fn send_message<M: Message>(send: &mut SendHalf, message: &M) -> Result<()> {
    let len = message.encoded_len();
    if len == 0 || len > MAX_FRAME_SIZE {
        return Err(NetError::Protocol(format!("frame of {len} bytes is empty or exceeds {MAX_FRAME_SIZE}")));
    }
    let mut encoded = Vec::with_capacity(4 + len);
    encoded.extend_from_slice(&(len as u32).to_be_bytes());
    message.encode(&mut encoded).map_err(|e| NetError::Protocol(e.to_string()))?;
    send.write_all(&encoded).await?;
    send.flush().await?;
    Ok(())
}

/// Reads one message written by [`send_message`]. Returns `None` only on a
/// clean EOF before the next frame; a truncated or oversized frame is an error.
pub async fn recv_message<M: Message + Default>(recv: &mut RecvHalf) -> Result<Option<M>> {
    let mut header = [0u8; 4];
    if recv.read(&mut header[..1]).await? == 0 {
        return Ok(None);
    }
    recv.read_exact(&mut header[1..]).await?;
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME_SIZE {
        return Err(NetError::Protocol(format!("invalid frame size: {len}")));
    }
    let mut data = vec![0; len];
    recv.read_exact(&mut data).await?;
    Ok(Some(M::decode(data.as_slice())?))
}

/// Sends one control frame. Never sends a partial oversize frame.
pub async fn send_frame(send: &mut SendHalf, frame: &ControlFrame) -> Result<()> {
    if frame.kind.is_none() {
        return Err(NetError::Protocol("empty control frame".into()));
    }
    send_message(send, frame).await
}

/// Returns `None` only on a clean stream EOF before the next frame. A
/// truncated header/payload or an oversized advertised size is an error.
pub async fn recv_frame(recv: &mut RecvHalf) -> Result<Option<ControlFrame>> {
    let Some(frame) = recv_message::<ControlFrame>(recv).await? else { return Ok(None) };
    if frame.kind.is_none() {
        return Err(NetError::Protocol("empty control frame".into()));
    }
    Ok(Some(frame))
}

/// Both sides send first, then read. Hello carries no key: the caller knows the
/// remote key proven by the connection handshake.
pub async fn exchange_hello(stream: &mut Stream, local: Hello) -> Result<Hello> {
    send_frame(&mut stream.send, &control_frame::Kind::Hello(local).into()).await?;
    let received = recv_frame(&mut stream.recv).await?
        .ok_or_else(|| NetError::Protocol("peer closed before hello".into()))?;
    let Some(control_frame::Kind::Hello(hello)) = received.kind else {
        return Err(NetError::Protocol("first control frame must be hello".into()));
    };
    if hello.app_version.is_empty() || hello.protocols.iter().any(|p| p.name.is_empty() || p.version == 0) {
        return Err(NetError::Protocol("invalid peer hello".into()));
    }
    Ok(hello)
}

/// Records a hello from the authenticated `remote` on its peer row. Unknown
/// nodes are not added: connecting to a node never makes the caller its peer.
/// Returns whether the node is a known peer.
pub fn record_hello(peers: &PeerStore, remote: &NodeId, hello: &Hello) -> Result<bool> {
    peers.update_app_version(remote, &hello.app_version)
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
    async fn two_nodes_exchange_hello_and_only_known_peers_are_recorded() {
        let (mut a, mut b) = stream_pair();
        let a_store = PeerStore::in_memory().unwrap();
        let b_store = PeerStore::in_memory().unwrap();
        let a_key = key(1);
        let b_key = key(2);
        a_store.add_peer(&b_key.to_string(), None, &[], crate::peers::DiscoverySource::Manual).unwrap();
        let a_hello = Hello { app_version: "1.0".into(), protocols: vec![ProtocolVersion { name: "omp/control".into(), version: 1 }], name: "a".into() };
        let b_hello = Hello { app_version: "2.3".into(), protocols: vec![ProtocolVersion { name: "omp/session".into(), version: 2 }], name: "b".into() };
        let (a_result, b_result) = tokio::join!(
            exchange_hello(&mut a, a_hello.clone()),
            exchange_hello(&mut b, b_hello.clone()),
        );
        let (a_result, b_result) = (a_result.unwrap(), b_result.unwrap());
        assert_eq!(a_result, b_hello);
        assert_eq!(b_result, a_hello);
        assert!(record_hello(&a_store, &b_key, &a_result).unwrap());
        assert!(!record_hello(&b_store, &a_key, &b_result).unwrap());
        assert_eq!(a_store.lookup(&b_key).unwrap().unwrap().app_version.as_deref(), Some("2.3"));
        assert!(b_store.list().unwrap().is_empty(), "a dialer never becomes a peer by connecting");
        record_presence(&a_store, &b_key, &Presence { online: true, last_seen_ms: Some(123) }).unwrap();
        assert_eq!(a_store.lookup(&b_key).unwrap().unwrap().last_seen_ms, Some(123));
    }

    #[tokio::test]
    async fn grant_messages_round_trip_and_frame_bounds_are_enforced() {
        let (mut a, mut b) = stream_pair();
        let messages = [
            control_frame::Kind::GrantDelivery(GrantDelivery { token: vec![1, 2, 3], root: vec![9; 32] }),
            control_frame::Kind::GrantRenewal(GrantRenewal { grant_id: vec![4, 5] }),
            control_frame::Kind::GrantRenewalResult(GrantRenewalResult { token: None, refusal: "revoked".into() }),
            control_frame::Kind::GrantRevocation(GrantRevocation { grant_id: vec![4, 5] }),
            control_frame::Kind::Ack(Ack { error: String::new() }),
        ];
        for kind in messages {
            let expected = ControlFrame { kind: Some(kind) };
            send_frame(&mut a.send, &expected).await.unwrap();
            assert_eq!(recv_frame(&mut b.recv).await.unwrap(), Some(expected));
        }
        assert!(send_frame(&mut a.send, &ControlFrame { kind: Some(control_frame::Kind::GrantDelivery(GrantDelivery { token: vec![0; MAX_FRAME_SIZE], root: Vec::new() })) }).await.is_err());
        a.send.write_all(&((MAX_FRAME_SIZE + 1) as u32).to_be_bytes()).await.unwrap();
        assert!(matches!(recv_frame(&mut b.recv).await, Err(NetError::Protocol(_))));
    }
}
