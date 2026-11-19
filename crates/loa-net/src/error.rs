use crate::alpn::Alpn;
use crate::NodeId;

/// Errors of the network layer.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("binding the endpoint: {0}")]
    Bind(String),
    #[error("connecting to {peer}: {reason}")]
    Connect { peer: NodeId, reason: String },
    /// The peer shares no version of the protocol with us.
    #[error("{peer} speaks no shared version of {protocol}")]
    NoSharedVersion { peer: NodeId, protocol: String },
    /// The remote side presented a different node key than the one dialed.
    #[error("expected node {expected}, the peer proved {actual}")]
    WrongPeer { expected: NodeId, actual: NodeId },
    #[error("tls: {0}")]
    Tls(String),
    #[error("connection closed: {0}")]
    Closed(String),
    #[error("stream: {0}")]
    Stream(String),
    #[error("datagram of {size} bytes exceeds the limit of {max}")]
    DatagramTooLarge { size: usize, max: usize },
    #[error("datagram: {0}")]
    Datagram(String),
    /// The authorization port refused a connection.
    #[error("{peer} is not allowed on {alpn}: {reason}")]
    Denied { peer: NodeId, alpn: Alpn, reason: String },
    #[error("invalid alpn {0:?}")]
    InvalidAlpn(String),
    #[error("invalid node key: {0}")]
    InvalidKey(String),
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("timed out: {0}")]
    Timeout(String),
    #[error("decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("mdns: {0}")]
    Mdns(String),
    #[error("tailscale: {0}")]
    Tailscale(String),
}

pub type Result<T, E = NetError> = std::result::Result<T, E>;
