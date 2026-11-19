//! Reassembly of `rpc_chunk` sequences (protocol v2).

use crate::event::Chunk;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

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

impl Reassembler {
    /// `max_bytes` is the advertised `maxReassembledFrameBytes`.
    pub fn new(max_bytes: u64) -> Self {
        Self { max_bytes, partial: None }
    }

    /// Whether a sequence is open.
    pub fn in_progress(&self) -> bool {
        self.partial.is_some()
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
        let size = payload.len().div_ceil(parts);
        let pieces: Vec<&[u8]> = payload.chunks(size).collect();
        pieces
            .iter()
            .enumerate()
            .map(|(i, piece)| Chunk {
                chunk_id: id.to_owned(),
                index: i as u32,
                count: pieces.len() as u32,
                byte_length: payload.len() as u64,
                data: STANDARD.encode(piece),
            })
            .collect()
    }

    #[test]
    fn reassembles_a_split_frame_in_order() {
        let payload = br#"{"type":"response","command":"get_messages","success":true,"data":{"messages":[]}}"#;
        let mut r = Reassembler::new(1 << 20);
        let parts = chunks("rpc-1", payload, 3);
        assert_eq!(r.push(&parts[0]).unwrap(), None);
        assert_eq!(r.push(&parts[1]).unwrap(), None);
        assert_eq!(r.push(&parts[2]).unwrap().as_deref(), Some(&payload[..]));
        assert!(!r.in_progress());
    }

    #[test]
    fn rejects_gaps_interleaving_and_wrong_lengths() {
        let payload = b"0123456789abcdef";
        let mut r = Reassembler::new(1 << 20);
        let a = chunks("a", payload, 2);
        let b = chunks("b", payload, 2);
        assert!(matches!(r.push(&a[1]), Err(ChunkError::OutOfOrder { .. })));
        r.push(&a[0]).unwrap();
        assert!(matches!(r.push(&b[1]), Err(ChunkError::OutOfOrder { .. })));
        assert!(!r.in_progress(), "an error drops the open sequence");

        let mut lying = chunks("c", payload, 2);
        for c in &mut lying {
            c.byte_length = 99;
        }
        r.push(&lying[0]).unwrap();
        assert!(matches!(r.push(&lying[1]), Err(ChunkError::Length { .. })));

        let mut small = Reassembler::new(4);
        assert_eq!(small.push(&chunks("d", payload, 2)[0]), Err(ChunkError::TooLarge(16)));
    }
}
