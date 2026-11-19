//! Record framing: `[len u32 LE][crc32 u32 LE][len bytes of proto::Patch]`.

use crate::error::{Error, IoContext, Result};
use crate::proto;
use loa_core::Patch;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub(crate) const HEADER_LEN: u64 = 8;
/// Largest payload a record may hold. A larger length field is damage.
pub(crate) const MAX_PAYLOAD: u32 = 1 << 28;
/// A sparse offset index entry is kept every this many records.
pub(crate) const CHECKPOINT_EVERY: u64 = 1024;

/// Encodes `patch` as one framed record into `buf`, replacing its contents.
pub(crate) fn encode(patch: &Patch, buf: &mut Vec<u8>) -> Result<()> {
    let len = proto::encoded_patch_len(patch);
    let len32 = u32::try_from(len).ok().filter(|l| *l <= MAX_PAYLOAD).ok_or(Error::TooLarge(len))?;
    buf.clear();
    buf.reserve(HEADER_LEN as usize + len);
    buf.extend_from_slice(&[0; HEADER_LEN as usize]);
    proto::encode_patch(patch, buf);
    let crc = crc32fast::hash(&buf[HEADER_LEN as usize..]);
    buf[..4].copy_from_slice(&len32.to_le_bytes());
    buf[4..8].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

fn split_header(h: [u8; HEADER_LEN as usize]) -> (u32, u32) {
    let [a, b, c, d, e, f, g, i] = h;
    (u32::from_le_bytes([a, b, c, d]), u32::from_le_bytes([e, f, g, i]))
}

/// Reads records forward from a known record boundary up to `end`.
pub(crate) struct Reader {
    path: PathBuf,
    reader: BufReader<File>,
    offset: u64,
    end: u64,
    next_seq: u64,
    buf: Vec<u8>,
}

impl Reader {
    /// A reader at `offset`, which must start the record with sequence `next_seq`.
    pub(crate) fn new(path: &Path, offset: u64, end: u64, next_seq: u64) -> Result<Self> {
        let mut file = File::open(path).at(path)?;
        file.seek(SeekFrom::Start(offset)).at(path)?;
        let reader = BufReader::with_capacity(64 * 1024, file);
        Ok(Self { path: path.to_owned(), reader, offset, end, next_seq, buf: Vec::new() })
    }

    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    pub(crate) fn next_seq(&self) -> u64 {
        self.next_seq
    }

    fn corrupt(&self, reason: impl Into<String>) -> Error {
        Error::Corrupt { path: self.path.clone(), offset: self.offset, reason: reason.into() }
    }

    /// Reads a record header that must fit before `end`. Returns `(len, crc)`.
    fn read_frame(&mut self) -> Result<(u32, u32)> {
        if self.end - self.offset < HEADER_LEN {
            return Err(self.corrupt("record header runs past the end"));
        }
        let mut h = [0; HEADER_LEN as usize];
        self.reader.read_exact(&mut h).at(&self.path)?;
        let (len, crc) = split_header(h);
        if len == 0 || len > MAX_PAYLOAD || u64::from(len) > self.end - self.offset - HEADER_LEN {
            return Err(self.corrupt(format!("bad record length {len}")));
        }
        Ok((len, crc))
    }

    /// Reads, checks and decodes the next record. `None` at `end`.
    pub(crate) fn next_patch(&mut self) -> Result<Option<Patch>> {
        if self.offset >= self.end {
            return Ok(None);
        }
        let (len, crc) = self.read_frame()?;
        self.buf.resize(len as usize, 0);
        self.reader.read_exact(&mut self.buf).at(&self.path)?;
        if crc32fast::hash(&self.buf) != crc {
            return Err(self.corrupt("checksum mismatch"));
        }
        let patch = proto::decode_patch(&self.buf).map_err(|e| self.corrupt(e.to_string()))?;
        if patch.seq != self.next_seq {
            return Err(self.corrupt(format!("expected seq {}, found {}", self.next_seq, patch.seq)));
        }
        self.offset += HEADER_LEN + u64::from(len);
        self.next_seq += 1;
        Ok(Some(patch))
    }

    /// Steps over the next record without checking its payload.
    pub(crate) fn skip(&mut self) -> Result<()> {
        let (len, _) = self.read_frame()?;
        self.reader.seek_relative(i64::from(len)).at(&self.path)?;
        self.offset += HEADER_LEN + u64::from(len);
        self.next_seq += 1;
        Ok(())
    }
}

/// What [`scan`] found between its start and the end of the file.
pub(crate) struct Scan {
    /// Offset after the last good record.
    pub end: u64,
    /// Sequence of the last good record, or the one before the start.
    pub last_seq: u64,
    /// Start offset of the last good record found by the scan.
    pub last_record: Option<u64>,
    /// `(seq, offset)` of every [`CHECKPOINT_EVERY`]th record.
    pub checkpoints: Vec<(u64, u64)>,
    /// True when bytes after `end` are a torn write.
    pub torn: bool,
}

/// Checks every record from `start` (which holds `next_seq`) to `file_len`.
///
/// A damaged record is a torn tail when nothing but zeros follows it, or when
/// its declared length runs past the end of the file: those are the shapes an
/// interrupted append leaves. Damage with real data after it is
/// [`Error::Corrupt`].
pub(crate) fn scan(path: &Path, file_len: u64, start: u64, next_seq: u64) -> Result<Scan> {
    let mut reader = Reader::new(path, start, file_len, next_seq)?;
    let mut out =
        Scan { end: start, last_seq: next_seq - 1, last_record: None, checkpoints: Vec::new(), torn: false };
    loop {
        let off = reader.offset;
        if off == file_len {
            return Ok(out);
        }
        let rem = file_len - off;
        if rem < HEADER_LEN {
            out.torn = true;
            return Ok(out);
        }
        let mut h = [0; HEADER_LEN as usize];
        reader.reader.read_exact(&mut h).at(path)?;
        let (len, crc) = split_header(h);
        let extent = HEADER_LEN + u64::from(len);
        if extent > rem {
            out.torn = true;
            return Ok(out);
        }
        let bad_len = len == 0 || len > MAX_PAYLOAD;
        let damaged = bad_len || {
            reader.buf.resize(len as usize, 0);
            reader.reader.read_exact(&mut reader.buf).at(path)?;
            crc32fast::hash(&reader.buf) != crc
        };
        if damaged {
            // A bad length tells nothing about where the record ends, so
            // only zeros from its start count as a torn write.
            let tail_from = if bad_len { off } else { off + extent };
            if zeros_from(path, tail_from)? {
                out.torn = true;
                return Ok(out);
            }
            return Err(reader.corrupt(if bad_len { format!("bad record length {len}") } else { "checksum mismatch".into() }));
        }
        let patch = proto::decode_patch(&reader.buf).map_err(|e| reader.corrupt(e.to_string()))?;
        if patch.seq != reader.next_seq {
            return Err(reader.corrupt(format!("expected seq {}, found {}", reader.next_seq, patch.seq)));
        }
        if (patch.seq - 1).is_multiple_of(CHECKPOINT_EVERY) {
            out.checkpoints.push((patch.seq, off));
        }
        reader.offset += extent;
        reader.next_seq += 1;
        out.end = reader.offset;
        out.last_seq = patch.seq;
        out.last_record = Some(off);
    }
}

/// True when every byte from `offset` to the end of the file is zero.
fn zeros_from(path: &Path, offset: u64) -> Result<bool> {
    let mut file = File::open(path).at(path)?;
    file.seek(SeekFrom::Start(offset)).at(path)?;
    let mut chunk = [0u8; 8192];
    loop {
        let n = file.read(&mut chunk).at(path)?;
        if n == 0 {
            return Ok(true);
        }
        if chunk[..n].iter().any(|b| *b != 0) {
            return Ok(false);
        }
    }
}

/// Reads the record at `offset` and returns its patch sequence and end offset,
/// or `None` when it isn't a whole, valid record inside `file_len`.
pub(crate) fn probe(path: &Path, offset: u64, file_len: u64) -> Result<Option<(u64, u64)>> {
    if offset >= file_len {
        return Ok(None);
    }
    let mut file = File::open(path).at(path)?;
    file.seek(SeekFrom::Start(offset)).at(path)?;
    let mut h = [0; HEADER_LEN as usize];
    if file_len - offset < HEADER_LEN {
        return Ok(None);
    }
    file.read_exact(&mut h).at(path)?;
    let (len, crc) = split_header(h);
    let end = offset + HEADER_LEN + u64::from(len);
    if len == 0 || len > MAX_PAYLOAD || end > file_len {
        return Ok(None);
    }
    let mut payload = vec![0; len as usize];
    file.read_exact(&mut payload).at(path)?;
    if crc32fast::hash(&payload) != crc {
        return Ok(None);
    }
    Ok(proto::decode_patch(&payload).ok().map(|p| (p.seq, end)))
}
