//! One length-prefixed prost codec (4-byte big-endian length, at most
//! [`MAX_FRAME_SIZE`]) and the messages of every node protocol. The
//! authenticated transport supplies the peer key; no message carries one.

use crate::transport::{RecvHalf, SendHalf, Stream};
use crate::{NodeError, Result};
use loa_trust::Action;
use prost::{Message, Oneof};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The protocol labels this build serves; dialing checks the exact label.
pub const CONTROL: &[u8] = b"omp/control/1";
pub const SESSION: &[u8] = b"omp/session/1";
pub const FOLDER: &[u8] = b"omp/folder/1";
/// Every label in the order a hello announces them.
pub const LOCAL: [&[u8]; 3] = [CONTROL, SESSION, FOLDER];
/// Largest encoded message, without its length prefix.
pub const MAX_FRAME_SIZE: usize = 1024 * 1024;

/// Sends one message; an empty or oversized one is refused before writing.
pub async fn send<M: Message>(tx: &mut SendHalf, message: &M) -> Result<()> {
    let len = message.encoded_len();
    if len == 0 || len > MAX_FRAME_SIZE {
        return Err(NodeError::Protocol(format!("frame of {len} bytes is empty or exceeds {MAX_FRAME_SIZE}")));
    }
    let mut bytes = (len as u32).to_be_bytes().to_vec();
    message.encode(&mut bytes).map_err(|e| NodeError::Protocol(e.to_string()))?;
    tx.write_all(&bytes).await?;
    Ok(tx.flush().await?)
}

/// The next message; `None` only on a clean end of stream before it.
pub async fn recv<M: Message + Default>(rx: &mut RecvHalf) -> Result<Option<M>> {
    let mut header = [0u8; 4];
    if rx.read(&mut header[..1]).await? == 0 {
        return Ok(None);
    }
    rx.read_exact(&mut header[1..]).await?;
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > MAX_FRAME_SIZE {
        return Err(NodeError::Protocol(format!("invalid frame size: {len}")));
    }
    let mut data = vec![0; len];
    rx.read_exact(&mut data).await?;
    Ok(Some(M::decode(data.as_slice())?))
}

/// A protocol's answer type: it can carry an acknowledgement or a refusal.
pub trait Envelope: Message + Default {
    /// An acknowledgement, a refusal when `error` is set.
    fn ack(error: Option<String>) -> Self;
    /// `Err(Refused)` for a refusal, the message otherwise.
    fn into_ack(self) -> Result<Self>;
}

/// The next answer, with refusals and an early end as errors.
pub async fn expect<M: Envelope>(rx: &mut RecvHalf) -> Result<M> {
    recv::<M>(rx).await?.ok_or_else(|| NodeError::Protocol("the stream ended early".into()))?.into_ack()
}

/// Sends `request` and waits for its answer.
pub async fn call<Req: Message, Rep: Envelope>(stream: &mut Stream, request: &Req) -> Result<Rep> {
    send(&mut stream.send, request).await?;
    expect(&mut stream.recv).await
}

/// Sends an acknowledgement, or a refusal when `error` is set.
pub async fn ack<M: Envelope>(tx: &mut SendHalf, error: Option<String>) -> Result<()> { send(tx, &M::ack(error)).await }

fn refusal(error: String) -> Result<()> { if error.is_empty() { Ok(()) } else { Err(NodeError::Refused(error)) } }

/// Defines a prost message with public fields.
macro_rules! messages {
    ($($(#[$meta:meta])* $name:ident { $($(#[$f:meta])* $field:ident: $ty:ty),* $(,)? })*) => {$(
        $(#[$meta])* #[derive(Clone, PartialEq, Message)]
        pub struct $name { $($(#[$f])* pub $field: $ty),* }
    )*};
}

/// Defines a oneof frame `$frame { body: Option<$body> }` and its [`Envelope`] over `$ack`.
macro_rules! frame {
    ($frame:ident, $body:ident, $name:literal, $tags:literal { $($variant:ident($ty:ty) = $tag:literal),* $(,)? }) => {
        #[derive(Clone, PartialEq, Message)]
        pub struct $frame { #[prost(oneof = $name, tags = $tags)] pub body: Option<$body> }
        #[derive(Clone, PartialEq, Oneof)]
        pub enum $body { $(#[prost(message, tag = $tag)] $variant($ty)),* }
        impl From<$body> for $frame { fn from(body: $body) -> Self { Self { body: Some(body) } } }
        impl Envelope for $frame {
            fn ack(error: Option<String>) -> Self { $body::Ack(Ack { error: error.unwrap_or_default() }).into() }
            fn into_ack(self) -> Result<Self> {
                match self.body {
                    Some($body::Ack(Ack { error })) if !error.is_empty() => Err(NodeError::Refused(error)),
                    None => Err(NodeError::Protocol("empty frame".into())),
                    _ => Ok(self),
                }
            }
        }
    };
}

messages! {
    /// Accepted when `error` is empty.
    Ack { #[prost(string, tag = "1")] error: String }
    ProtocolVersion { #[prost(string, tag = "1")] name: String, #[prost(uint32, tag = "2")] version: u32 }
    /// Both sides send it first on a control stream; `name` is a display hint.
    Hello {
        #[prost(string, tag = "1")] app_version: String,
        #[prost(message, repeated, tag = "2")] protocols: Vec<ProtocolVersion>,
        #[prost(string, tag = "3")] name: String,
    }
    Presence { #[prost(bool, tag = "1")] online: bool, #[prost(int64, optional, tag = "2")] last_seen_ms: Option<i64> }
    /// An opaque Biscuit; `root` is empty when the sender minted it.
    GrantDelivery { #[prost(bytes = "vec", tag = "1")] token: Vec<u8>, #[prost(bytes = "vec", tag = "2")] root: Vec<u8> }
    GrantRenewal { #[prost(bytes = "vec", tag = "1")] grant_id: Vec<u8> }
    GrantRenewalResult { #[prost(bytes = "vec", optional, tag = "1")] token: Option<Vec<u8>>, #[prost(string, tag = "2")] refusal: String }
    GrantRevocation { #[prost(bytes = "vec", tag = "1")] grant_id: Vec<u8> }

    /// A nonce the requester signs over each grant it presents.
    Challenge { #[prost(bytes = "vec", tag = "1")] nonce: Vec<u8> }
    PresentedGrant { #[prost(bytes = "vec", tag = "1")] token: Vec<u8>, #[prost(bytes = "vec", tag = "2")] proof: Vec<u8> }
    Present { #[prost(message, repeated, tag = "1")] grants: Vec<PresentedGrant> }
    ListSessions {}
    SessionEntry {
        #[prost(string, tag = "1")] id: String,
        /// Empty unless the grant covers the whole session.
        #[prost(string, tag = "2")] title: String,
        #[prost(string, tag = "3")] harness: String,
        #[prost(int64, tag = "4")] created_ms: i64,
        #[prost(string, repeated, tag = "5")] actions: Vec<String>,
        #[prost(bool, tag = "6")] partial: bool,
        #[prost(uint64, tag = "7")] last_seq: u64,
    }
    SessionList { #[prost(message, repeated, tag = "1")] sessions: Vec<SessionEntry> }
    /// The requester's visible DOM at the owner's `source_seq`, as patches from 1.
    Seed {
        #[prost(uint64, tag = "1")] source_seq: u64,
        #[prost(message, repeated, tag = "2")] patches: Vec<loa_bus::proto::Patch>,
        #[prost(string, repeated, tag = "3")] actions: Vec<String>,
    }
    DriveResult {
        #[prost(message, optional, tag = "1")] patch: Option<loa_bus::proto::Patch>,
        #[prost(uint64, optional, tag = "2")] conflict_current: Option<u64>,
        #[prost(string, tag = "3")] error: String,
    }
    Prompt { #[prost(string, tag = "1")] session: String, #[prost(string, tag = "2")] text: String }
    /// An answer to a waiting decision, by its id in the peer's view.
    Answer {
        #[prost(string, tag = "1")] session: String,
        #[prost(uint64, tag = "2")] decision: u64,
        #[prost(string, repeated, tag = "3")] values: Vec<String>,
        #[prost(string, optional, tag = "4")] reason: Option<String>,
    }
    Subscribe { #[prost(string, tag = "1")] session: String, #[prost(uint64, tag = "2")] after_source_seq: u64 }
    WireUpdate { #[prost(uint64, tag = "1")] source_seq: u64, #[prost(message, optional, tag = "2")] patch: Option<loa_bus::proto::Patch> }
    /// A draft (JSON) against the owner's `base_seq`.
    Drive {
        #[prost(string, tag = "1")] session: String,
        #[prost(uint64, tag = "2")] base_seq: u64,
        #[prost(bytes = "vec", tag = "3")] draft: Vec<u8>,
        #[prost(int64, tag = "4")] ts: i64,
    }

    /// A folder request (`list`, `read`, `write`) and each later chunk of a write (`data` only).
    FolderRequest {
        #[prost(string, tag = "1")] op: String,
        #[prost(string, tag = "2")] folder: String,
        #[prost(string, tag = "3")] path: String,
        #[prost(uint64, tag = "4")] size: u64,
        #[prost(bytes = "vec", tag = "5")] data: Vec<u8>,
    }
    WireEntry { #[prost(string, tag = "1")] name: String, #[prost(bool, tag = "2")] dir: bool, #[prost(uint64, tag = "3")] size: u64 }
    /// Refused (`error`), go ahead (`ready`), then the result; a read's last chunk is `done`.
    FolderReply {
        #[prost(string, tag = "1")] error: String,
        #[prost(bool, tag = "2")] ready: bool,
        #[prost(message, repeated, tag = "3")] entries: Vec<WireEntry>,
        #[prost(bytes = "vec", tag = "4")] data: Vec<u8>,
        #[prost(bool, tag = "5")] done: bool,
    }
}

frame!(ControlFrame, Control, "Control", "1, 2, 3, 4, 5, 6, 7" {
    Hello(Hello) = "1", Presence(Presence) = "2", GrantDelivery(GrantDelivery) = "3", GrantRenewal(GrantRenewal) = "4",
    GrantRenewalResult(GrantRenewalResult) = "5", GrantRevocation(GrantRevocation) = "6", Ack(Ack) = "7",
});

frame!(Frame, Body, "Body", "1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12" {
    Subscribe(Subscribe) = "1", Update(WireUpdate) = "2", Drive(Drive) = "3", Challenge(Challenge) = "4",
    Present(Present) = "5", ListSessions(ListSessions) = "6", SessionList(SessionList) = "7", Seed(Seed) = "8",
    DriveResult(DriveResult) = "9", Prompt(Prompt) = "10", Ack(Ack) = "11", Answer(Answer) = "12",
});

impl Envelope for FolderReply {
    fn ack(error: Option<String>) -> Self { Self { done: error.is_some(), ready: error.is_none(), error: error.unwrap_or_default(), ..Self::default() } }
    fn into_ack(self) -> Result<Self> { refusal(self.error.clone()).map(|()| self) }
}

impl Seed {
    pub fn new(source_seq: u64, patches: &[loa_core::Patch], actions: &[Action]) -> Self {
        Self { source_seq, patches: patches.iter().map(Into::into).collect(), actions: actions.iter().map(|a| a.as_str().to_owned()).collect() }
    }
}

/// Decodes wire patches.
pub fn patches(wire: Vec<loa_bus::proto::Patch>) -> Result<Vec<loa_core::Patch>> {
    wire.into_iter().map(TryInto::try_into).collect::<Result<_, loa_bus::proto::ProtoError>>().map_err(|e| NodeError::Protocol(e.to_string()))
}

/// Parses action names, skipping unknown ones.
pub fn actions(names: &[String]) -> Vec<Action> { names.iter().filter_map(|a| Action::parse(a)).collect() }

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip_and_bounds_are_enforced() {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let ((ar, aw), (br, bw)) = (tokio::io::split(a), tokio::io::split(b));
        let (mut a, mut b) = (Stream { send: Box::new(aw), recv: Box::new(ar) }, Stream { send: Box::new(bw), recv: Box::new(br) });
        let delivery = ControlFrame::from(Control::GrantDelivery(GrantDelivery { token: vec![1, 2], root: vec![9; 32] }));
        send(&mut a.send, &delivery).await.unwrap();
        assert_eq!(recv::<ControlFrame>(&mut b.recv).await.unwrap(), Some(delivery));
        ack::<Frame>(&mut a.send, Some("no".into())).await.unwrap();
        assert!(matches!(expect::<Frame>(&mut b.recv).await, Err(NodeError::Refused(e)) if e == "no"));
        assert!(send(&mut a.send, &GrantDelivery { token: vec![0; MAX_FRAME_SIZE], root: Vec::new() }).await.is_err());
        a.send.write_all(&((MAX_FRAME_SIZE + 1) as u32).to_be_bytes()).await.unwrap();
        assert!(matches!(recv::<Frame>(&mut b.recv).await, Err(NodeError::Protocol(_))));
    }
}
