//! The app's peer node: authenticated transports (iroh QUIC, TLS+yamux TCP),
//! LAN and tailnet discovery, grants delivered over `omp/control/1`, and
//! session and folder sharing over `omp/session/1` and `omp/folder/1`.

mod auth;
pub mod discovery;
mod node;
mod peers_db;
mod protocols;
mod sessions;
pub mod transport;
pub mod wire;

pub use auth::{GrantAccess, GrantDirection, GrantGate, GrantInfo};
pub use discovery::{Availability, Discoverer, NearbyPeer, NearbySource};
pub use loa_settings::app::DEFAULT_PORT;
pub use loa_trust::KeySource;
pub use node::{Endpoint, Node, NodeConfig, NodeEvent, NodeStatus, SANDBOX_LABEL, data_dir, local_protocols};
pub use peers_db::{DiscoverySource, PeerInfo};
pub use protocols::extra::{AddedProtocol, NodeSlot, PeerProtocol};
pub use protocols::folder::{FolderEntry, MAX_FILE as MAX_FOLDER_FILE, SharedFolder};
pub use protocols::projection::{Access, Mirror, Projection, Update, fork_mirror_to_own_node};
pub use protocols::session::{DriveOutcome, RemoteSession, RemoteView};
pub use sessions::{BusSessions, LocalSession, LocalSessions, LocalTurn};
pub use transport::{Connection, RelayConfig, Stream, TransportKind};

/// The public ed25519 node key, the network address.
pub type NodeId = iroh::PublicKey;

/// Every error of the node, its transports and its protocols.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Net(String),
    #[error("connecting to {0}: {1}")]
    Connect(NodeId, String),
    #[error("expected node {0}, the peer proved {1}")]
    WrongPeer(NodeId, NodeId),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Grant(#[from] loa_trust::TrustError),
    #[error("journal: {0}")]
    Journal(#[from] loa_bus::JournalError),
    #[error("DOM: {0}")]
    Apply(#[from] loa_core::ApplyError),
    #[error("request: {0}")]
    Request(#[from] loa_bus::RequestError),
    #[error("replication: {0}")]
    Replicate(#[from] loa_bus::ReplicateError),
    #[error("patch {got} is not next after {last}")]
    Sequence { last: u64, got: u64 },
    #[error("not a node key: {0:?}")]
    InvalidKey(String),
    #[error("invalid address {0:?}: {1}")]
    InvalidAddress(String, String),
    #[error("unknown peer {0}")]
    UnknownPeer(String),
    #[error("the peer endpoint is not running")]
    NotStarted,
    #[error("this node does not own the session")]
    NotOwner,
    #[error("the peer refused: {0}")]
    Refused(String),
    #[error("{0} timed out")]
    Timeout(&'static str),
    #[error("{0}")]
    Unavailable(String),
}

pub type Result<T, E = NodeError> = std::result::Result<T, E>;

/// Maps any displayable error into [`NodeError::Net`].
pub(crate) fn net(error: impl std::fmt::Display) -> NodeError { NodeError::Net(error.to_string()) }
