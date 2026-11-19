//! Line framed JSON transport to an `omp --mode rpc` child process.

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

use crate::{Error, Result};

/// Lines of omp's stderr kept for the error reported when it exits.
const STDERR_TAIL_LINES: usize = 20;

/// Reassembles protocol v2 `rpc_chunk` sequences into the logical frames they carry.
pub(crate) struct ChunkAssembler {
    max_bytes: usize,
    partial: Option<Partial>,
}

struct Partial {
    chunk_id: String,
    count: u64,
    next: u64,
    byte_length: usize,
    bytes: Vec<u8>,
}

impl ChunkAssembler {
    /// `max_bytes` is the reassembly limit omp advertises in its ready frame.
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            partial: None,
        }
    }

    pub(crate) fn set_max_bytes(&mut self, max_bytes: usize) {
        self.max_bytes = max_bytes;
    }

    /// Feeds one physical frame. Returns the logical frame once it is complete.
    pub(crate) fn push(&mut self, frame: Value) -> Result<Option<Value>> {
        if frame.get("type").and_then(Value::as_str) != Some("rpc_chunk") {
            if self.partial.is_some() {
                return Err(Error::Frame("chunk sequence interrupted by another frame".into()));
            }
            return Ok(Some(frame));
        }
        let field = |name: &str| {
            frame
                .get(name)
                .ok_or_else(|| Error::Frame(format!("rpc_chunk without `{name}`")))
        };
        let chunk_id = field("chunkId")?
            .as_str()
            .ok_or_else(|| Error::Frame("rpc_chunk `chunkId` is not a string".into()))?;
        let index = field("index")?
            .as_u64()
            .ok_or_else(|| Error::Frame("rpc_chunk `index` is not an integer".into()))?;
        let count = field("count")?
            .as_u64()
            .ok_or_else(|| Error::Frame("rpc_chunk `count` is not an integer".into()))?;
        let byte_length = field("byteLength")?
            .as_u64()
            .and_then(|length| usize::try_from(length).ok())
            .ok_or_else(|| Error::Frame("rpc_chunk `byteLength` is not a size".into()))?;
        let data = field("data")?
            .as_str()
            .ok_or_else(|| Error::Frame("rpc_chunk `data` is not a string".into()))?;

        if index == 0 {
            if self.partial.is_some() {
                return Err(Error::Frame("new chunk sequence before the last one ended".into()));
            }
            if count == 0 || byte_length > self.max_bytes {
                return Err(Error::Frame(format!(
                    "chunk sequence of {count} chunks and {byte_length} bytes is out of bounds"
                )));
            }
            self.partial = Some(Partial {
                chunk_id: chunk_id.to_owned(),
                count,
                next: 0,
                byte_length,
                bytes: Vec::with_capacity(byte_length),
            });
        }
        let Some(partial) = self.partial.as_mut() else {
            return Err(Error::Frame(format!("chunk {index} without a sequence start")));
        };
        if partial.chunk_id != chunk_id
            || partial.next != index
            || partial.count != count
            || partial.byte_length != byte_length
        {
            return Err(Error::Frame(format!(
                "chunk {chunk_id}#{index} does not continue {}#{}",
                partial.chunk_id, partial.next
            )));
        }
        base64::engine::general_purpose::STANDARD
            .decode_vec(data, &mut partial.bytes)
            .map_err(|error| Error::Frame(format!("rpc_chunk data: {error}")))?;
        if partial.bytes.len() > partial.byte_length {
            return Err(Error::Frame("chunk sequence longer than its byteLength".into()));
        }
        partial.next += 1;
        if partial.next < partial.count {
            return Ok(None);
        }
        let Some(done) = self.partial.take() else {
            return Err(Error::Frame("chunk sequence vanished".into()));
        };
        if done.bytes.len() != done.byte_length {
            return Err(Error::Frame("chunk sequence shorter than its byteLength".into()));
        }
        Ok(Some(serde_json::from_slice(&done.bytes)?))
    }
}

/// A running omp RPC process.
pub(crate) struct RpcChild {
    _child: Child,
    stdin: ChildStdin,
    frames: mpsc::Receiver<Result<Value>>,
    max_bytes: Arc<Mutex<usize>>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    line: Vec<u8>,
}

impl RpcChild {
    /// Spawns `command` with piped stdio. The process is killed when this value drops.
    pub(crate) fn spawn(mut command: Command) -> Result<Self> {
        let program = command.as_std().get_program().to_string_lossy().into_owned();
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|source| Error::Spawn { program, source })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(Error::Frame("child pipes missing after a piped spawn".into()));
        };

        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_LINES)));
        let tail = Arc::clone(&stderr_tail);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(mut tail) = tail.lock() else { return };
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });

        let max_bytes = Arc::new(Mutex::new(usize::MAX));
        let reader_max = Arc::clone(&max_bytes);
        let (sender, frames) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut assembler = ChunkAssembler::new(usize::MAX);
            let mut line = Vec::new();
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line).await {
                    Ok(0) => return,
                    Ok(_) => {}
                    Err(error) => {
                        let _ = sender.send(Err(Error::Io(error))).await;
                        return;
                    }
                }
                let text = line.trim_ascii();
                if text.is_empty() {
                    continue;
                }
                if let Ok(max) = reader_max.lock() {
                    assembler.set_max_bytes(*max);
                }
                let frame = serde_json::from_slice::<Value>(text)
                    .map_err(Error::from)
                    .and_then(|frame| assembler.push(frame));
                let stop = frame.is_err();
                match frame {
                    Ok(None) => continue,
                    Ok(Some(frame)) => {
                        if sender.send(Ok(frame)).await.is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error)).await;
                    }
                }
                if stop {
                    return;
                }
            }
        });

        Ok(Self {
            _child: child,
            stdin,
            frames,
            max_bytes,
            stderr_tail,
            line: Vec::new(),
        })
    }

    /// Sets the chunk reassembly limit from omp's ready frame.
    pub(crate) fn set_max_reassembled_bytes(&self, max: usize) {
        if let Ok(mut current) = self.max_bytes.lock() {
            *current = max;
        }
    }

    /// Writes one command frame.
    pub(crate) async fn send(&mut self, frame: &Value) -> Result<()> {
        self.line.clear();
        serde_json::to_writer(&mut self.line, frame)?;
        self.line.push(b'\n');
        let written = self.stdin.write_all(&self.line).await;
        let flushed = match written {
            Ok(()) => self.stdin.flush().await,
            Err(error) => Err(error),
        };
        match flushed {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Err(self.exited()),
            Err(error) => Err(Error::Io(error)),
        }
    }

    /// Next logical frame from omp. Cancel safe.
    pub(crate) async fn next(&mut self) -> Result<Value> {
        match self.frames.recv().await {
            Some(frame) => frame,
            None => Err(self.exited()),
        }
    }

    fn exited(&self) -> Error {
        let stderr = self
            .stderr_tail
            .lock()
            .map(|tail| tail.iter().map(String::as_str).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        Error::OmpExited { stderr }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunks(value: &Value, pieces: usize, id: &str) -> Vec<Value> {
        let bytes = serde_json::to_vec(value).unwrap();
        let size = bytes.len().div_ceil(pieces);
        bytes
            .chunks(size)
            .enumerate()
            .map(|(index, piece)| {
                json!({
                    "type": "rpc_chunk",
                    "chunkId": id,
                    "index": index,
                    "count": pieces,
                    "byteLength": bytes.len(),
                    "data": base64::engine::general_purpose::STANDARD.encode(piece),
                })
            })
            .collect()
    }

    #[test]
    fn oversized_tool_result_is_reassembled_in_order() {
        let frame = json!({"type": "tool_execution_end", "result": {"content": [{"type": "text", "text": "é".repeat(5000)}]}});
        let mut assembler = ChunkAssembler::new(1 << 20);
        let mut out = Vec::new();
        for chunk in chunks(&frame, 4, "rpc-1") {
            out.push(assembler.push(chunk).unwrap());
        }
        assert_eq!(out[..3], [None, None, None]);
        assert_eq!(out[3].as_ref(), Some(&frame));
        let plain = json!({"type": "agent_end"});
        assert_eq!(assembler.push(plain.clone()).unwrap(), Some(plain));
    }

    #[test]
    fn interleaved_or_interrupted_sequences_are_rejected() {
        let frame = json!({"type": "response", "data": "x".repeat(300)});
        let first = chunks(&frame, 3, "a");
        let second = chunks(&frame, 3, "b");

        let mut assembler = ChunkAssembler::new(1 << 20);
        assembler.push(first[0].clone()).unwrap();
        assert!(assembler.push(second[1].clone()).is_err());

        let mut assembler = ChunkAssembler::new(1 << 20);
        assembler.push(first[0].clone()).unwrap();
        assert!(assembler.push(json!({"type": "agent_end"})).is_err());

        let mut assembler = ChunkAssembler::new(1 << 20);
        assembler.push(first[0].clone()).unwrap();
        assert!(assembler.push(first[2].clone()).is_err());
    }

    #[test]
    fn sequences_over_the_advertised_limit_are_rejected() {
        let frame = json!({"type": "response", "data": "x".repeat(300)});
        let mut assembler = ChunkAssembler::new(100);
        assert!(assembler.push(chunks(&frame, 2, "a")[0].clone()).is_err());
    }
}
