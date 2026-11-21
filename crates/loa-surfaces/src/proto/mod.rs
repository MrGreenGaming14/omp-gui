//! Remote viewing (the app's `remote-video` feature): `omp/surface/1` and
//! live video for viewers across the network.
//!
//! - [`wire`]: the protobuf messages of `omp/surface/1`
//! - [`transport`]: the connection port (ordered stream plus datagrams) and an in-memory pair
//! - [`fragment`]: frames split into datagrams, reassembled, late ones dropped
//! - [`serve`] on the server, [`open`] on the viewer ([`PixelViewer`] or [`DocumentViewer`])
//! - [`video`]: live surfaces coded as H.264/HEVC for viewers that decode it

pub mod fragment;
mod server;
pub mod transport;
pub mod video;
mod viewer;
pub mod wire;

pub use server::serve;
pub use transport::{DatagramError, Datagrams, Transport, memory_pair};
pub use viewer::{DocumentViewer, InputSender, Opened, PixelViewer, ViewerEvent, ViewerOptions, open};
pub use wire::{RejectCode, VERSION};

use loa_contracts::surface::SurfaceError;

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error(transparent)]
    Surface(#[from] SurfaceError),
    #[error("stream: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed message: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("datagram: {0}")]
    Datagram(#[from] DatagramError),
    #[error("protocol violation: {0}")]
    Protocol(String),
    #[error("{len} bytes exceed the {max} byte limit")]
    TooLarge { len: u64, max: u64 },
    #[error("a {0} byte datagram limit leaves no room for frame data")]
    DatagramTooSmall(usize),
    #[error("a {len} byte frame needs more than {} fragments of {max_datagram} bytes", fragment::MAX_FRAGMENTS)]
    FrameTooLarge { len: usize, max_datagram: usize },
    #[error("the server rejected the surface ({code:?}): {reason}")]
    Rejected { code: RejectCode, reason: String },
    #[error("session closed: {0}")]
    Closed(String),
    #[error("document checksum mismatch")]
    Checksum,
    #[error("video: {0}")]
    Video(String),
}

pub type Result<T, E = ProtoError> = std::result::Result<T, E>;

