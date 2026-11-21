//! [`OmpFrames`]: omp's envelope on [`kit::rpc`](crate::kit::rpc). Commands
//! carry their `type` and a string `id`; responses name the command; host
//! callbacks are requests; oversized frames arrive as `rpc_chunk` sequences
//! (protocol v2) and are reassembled here.

use crate::kit::rpc::{Framing, Incoming, RpcError};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use loa_util::LockExt;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::sync::Mutex;

/// Reassembly limit used until omp advertises its own.
const DEFAULT_REASSEMBLY_LIMIT: u64 = 64 * 1024 * 1024;

/// One piece of an oversized frame.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Chunk {
    pub chunk_id: String,
    pub index: u32,
    pub count: u32,
    pub byte_length: u64,
    pub data: String,
}

/// The omp [`Framing`].
#[derive(Debug)]
pub struct OmpFrames(Mutex<Reassembler>);

impl Default for OmpFrames {
    fn default() -> Self {
        Self(Mutex::new(Reassembler::new(DEFAULT_REASSEMBLY_LIMIT)))
    }
}

impl OmpFrames {
    /// The frame a chunk completes, if it completes one.
    fn chunk(&self, msg: Json) -> Option<Json> {
        let pushed = Chunk::deserialize(&msg).map_err(|e| e.to_string()).and_then(|chunk| {
            let bytes = self.0.locked().push(&chunk).map_err(|e| e.to_string())?;
            bytes.map(|b| serde_json::from_slice(&b).map_err(|e| e.to_string())).transpose()
        });
        pushed.unwrap_or_else(|err| {
            tracing::warn!(%err, "dropped an invalid chunk sequence");
            None
        })
    }
}

impl Framing for OmpFrames {
    fn request(&self, id: &Json, method: &str, params: Json) -> Json {
        let mut frame = match params {
            Json::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        frame.entry("type").or_insert_with(|| method.into());
        frame.insert("id".to_owned(), id.clone());
        Json::Object(frame)
    }

    fn notification(&self, method: &str, params: Json) -> Json {
        let mut frame = self.request(&Json::Null, method, params);
        frame.as_object_mut().map(|frame| frame.remove("id"));
        frame
    }

    /// Host callbacks answer with their whole result frame; an error becomes
    /// an error tool result.
    fn response(&self, id: Json, result: Result<Json, RpcError>) -> Json {
        result.unwrap_or_else(|err| {
            json!({"type": "host_tool_result", "id": id, "isError": true, "result": {"content": [{"type": "text", "text": err.to_string()}]}})
        })
    }

    fn classify(&self, msg: Json) -> Option<Incoming> {
        let kind = msg["type"].as_str().unwrap_or_default().to_owned();
        if kind == "rpc_chunk" {
            let frame = self.chunk(msg)?;
            if frame["type"] == "rpc_chunk" {
                tracing::warn!("a reassembled frame was itself a chunk");
                return None;
            }
            return self.classify(frame);
        }
        if let Some(err) = self.0.locked().interrupt() {
            tracing::warn!(%err, "dropped an interrupted chunk sequence");
        }
        Some(match kind.as_str() {
            "ready" => {
                if let Some(limit) = msg["maxReassembledFrameBytes"].as_u64() {
                    *self.0.locked() = Reassembler::new(limit);
                }
                Incoming::Notification { method: kind, params: msg }
            }
            "response" if msg["id"].is_string() => {
                let id = msg["id"].as_str().unwrap_or_default().to_owned();
                let result = match msg["success"].as_bool() {
                    Some(true) => Ok(msg.get("data").cloned().unwrap_or(Json::Null)),
                    _ => Err(RpcError::Command {
                        command: msg["command"].as_str().unwrap_or_default().to_owned(),
                        message: msg["error"].as_str().unwrap_or_default().to_owned(),
                        code: msg["code"].as_str().map(str::to_owned),
                    }),
                };
                Incoming::Response { id, result }
            }
            "host_tool_call" | "host_uri_request" => Incoming::Request { id: msg["id"].clone(), method: kind, params: msg },
            "host_tool_cancel" | "host_uri_cancel" => Incoming::Cancel { id: msg["targetId"].clone() },
            _ => Incoming::Notification { method: kind, params: msg },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChunkError {
    #[error("chunk {index} of `{chunk_id}` arrived out of order")]
    OutOfOrder { chunk_id: String, index: u32 },
    #[error("chunk sequence `{0}` was interrupted by another frame")]
    Interrupted(String),
    #[error("chunk sequence declares {0} bytes, over the reassembly limit")]
    TooLarge(u64),
    #[error("chunk sequence `{chunk_id}` decoded to {got} bytes, declared {declared}")]
    Length { chunk_id: String, got: u64, declared: u64 },
    #[error("chunk data isn't valid base64")]
    Base64,
}

/// Collects one chunk sequence at a time, enforcing the rules of `rpc.md`:
/// no interleaving, no gaps, a declared length, and a size cap.
#[derive(Debug)]
pub struct Reassembler {
    max_bytes: u64,
    partial: Option<Partial>,
}

#[derive(Debug)]
struct Partial {
    chunk_id: String,
    count: u32,
    next: u32,
    byte_length: u64,
    bytes: Vec<u8>,
}

impl Reassembler {
    /// `max_bytes` is the advertised `maxReassembledFrameBytes`.
    pub fn new(max_bytes: u64) -> Self {
        Self { max_bytes, partial: None }
    }

    /// Drops an open sequence because a non-chunk frame interrupted it.
    pub fn interrupt(&mut self) -> Option<ChunkError> {
        self.partial.take().map(|p| ChunkError::Interrupted(p.chunk_id))
    }

    /// Adds one chunk. Returns the whole frame once the last chunk arrived.
    /// Any error drops the open sequence.
    pub fn push(&mut self, chunk: &Chunk) -> Result<Option<Vec<u8>>, ChunkError> {
        let result = self.push_inner(chunk);
        if result.is_err() {
            self.partial = None;
        }
        result
    }

    fn push_inner(&mut self, chunk: &Chunk) -> Result<Option<Vec<u8>>, ChunkError> {
        let partial = match &mut self.partial {
            None => {
                if chunk.index != 0 || chunk.count == 0 {
                    return Err(ChunkError::OutOfOrder { chunk_id: chunk.chunk_id.clone(), index: chunk.index });
                }
                if chunk.byte_length > self.max_bytes {
                    return Err(ChunkError::TooLarge(chunk.byte_length));
                }
                let capacity = usize::try_from(chunk.byte_length).map_err(|_| ChunkError::TooLarge(chunk.byte_length))?;
                self.partial.insert(Partial {
                    chunk_id: chunk.chunk_id.clone(),
                    count: chunk.count,
                    next: 0,
                    byte_length: chunk.byte_length,
                    bytes: Vec::with_capacity(capacity),
                })
            }
            Some(partial) => {
                if partial.chunk_id != chunk.chunk_id
                    || partial.next != chunk.index
                    || partial.count != chunk.count
                    || partial.byte_length != chunk.byte_length
                {
                    return Err(ChunkError::OutOfOrder { chunk_id: chunk.chunk_id.clone(), index: chunk.index });
                }
                partial
            }
        };
        STANDARD.decode_vec(chunk.data.as_bytes(), &mut partial.bytes).map_err(|_| ChunkError::Base64)?;
        let got = partial.bytes.len() as u64;
        if got > partial.byte_length {
            return Err(ChunkError::Length { chunk_id: partial.chunk_id.clone(), got, declared: partial.byte_length });
        }
        partial.next += 1;
        if partial.next < partial.count {
            return Ok(None);
        }
        let Some(done) = self.partial.take() else {
            return Ok(None);
        };
        if got != done.byte_length {
            return Err(ChunkError::Length { chunk_id: done.chunk_id, got, declared: done.byte_length });
        }
        Ok(Some(done.bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(id: &str, payload: &[u8], parts: usize) -> Vec<Chunk> {
        let pieces: Vec<&[u8]> = payload.chunks(payload.len().div_ceil(parts)).collect();
        let chunk = |(i, piece): (usize, &&[u8])| Chunk {
            chunk_id: id.to_owned(),
            index: i as u32,
            count: pieces.len() as u32,
            byte_length: payload.len() as u64,
            data: STANDARD.encode(piece),
        };
        pieces.iter().enumerate().map(chunk).collect()
    }

    #[test]
    fn reassembles_in_order_and_rejects_gaps_interleaving_and_wrong_lengths() {
        let payload = b"0123456789abcdef";
        let mut r = Reassembler::new(1 << 20);
        let parts = chunks("ok", payload, 3);
        assert_eq!(r.push(&parts[0]).unwrap(), None);
        assert_eq!(r.push(&parts[1]).unwrap(), None);
        assert_eq!(r.push(&parts[2]).unwrap().as_deref(), Some(&payload[..]));
        let (a, b) = (chunks("a", payload, 2), chunks("b", payload, 2));
        assert!(matches!(r.push(&a[1]), Err(ChunkError::OutOfOrder { .. })));
        r.push(&a[0]).unwrap();
        assert!(matches!(r.push(&b[1]), Err(ChunkError::OutOfOrder { .. })));
        assert_eq!(r.interrupt(), None, "an error drops the open sequence");
        let mut lying = chunks("c", payload, 2);
        lying.iter_mut().for_each(|c| c.byte_length = 99);
        r.push(&lying[0]).unwrap();
        assert!(matches!(r.push(&lying[1]), Err(ChunkError::Length { .. })));
        assert_eq!(Reassembler::new(4).push(&chunks("d", payload, 2)[0]), Err(ChunkError::TooLarge(16)));
    }
}
