//! Input to pixels latency benchmark and the 50 ms gate.
//!
//! - [`bench`] drives any [`loa_contracts::surface::Surface`]: it sends an input that
//!   changes the picture, then waits for the first frame that both echoes the
//!   input (`input_seq`) and shows the change in its pixels.
//! - [`synthetic`] is a software rendered reference surface, so the
//!   benchmark runs anywhere.
//! - [`encode`] turns RGBA pictures into frames: hardware or software H.264 and
//!   HEVC for live content, changed regions only for mostly static content.
//! - [`report`] summarises samples against the budget in `surfaces.md`.
//! - [`live`] codes live surfaces as video for viewers that negotiate it;
//!   [`video`] decodes it.

pub mod bench;
pub mod canvas;
pub mod decode;
pub mod encode;
pub mod live;
pub mod report;
pub mod stats;
pub mod synthetic;
pub mod yuv;
pub mod video;

use loa_contracts::surface::Encoding;

#[derive(Debug, thiserror::Error)]
pub enum BenchError {
    #[error(transparent)]
    Surface(#[from] loa_contracts::surface::SurfaceError),
    #[error("encoder: {0}")]
    Encode(String),
    #[error("decoding a {encoding:?} frame: {message}")]
    Decode { encoding: Encoding, message: String },
    #[error("no decoder for {0:?} frames")]
    NoDecoder(Encoding),
    #[error("{width}x{height} doesn't fit: {reason}")]
    Size { width: u32, height: u32, reason: &'static str },
    #[error("iteration {iteration}: no frame showed input {input_seq} within {timeout_ms} ms")]
    Timeout { iteration: usize, input_seq: u64, timeout_ms: u64 },
    #[error("the surface sent no full frame within {0} ms")]
    NoFirstFrame(u64),
    #[error("the frame stream closed")]
    StreamClosed,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T, E = BenchError> = std::result::Result<T, E>;
