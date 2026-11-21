//! The one framing codec of journal files.
//!
//! - A record is `[len u32 LE][crc32 u32 LE][len bytes]`; journal records
//!   hold one `proto::Patch` each.
//! - A file starts with a preamble, `[magic; 8][version u32 LE]`. The journal
//!   header is the preamble plus one record holding a prost message
//!   ([`FramedHeader`]); the snapshot header is the preamble plus fixed LE
//!   fields and a crc32 of everything before it.

use crate::error::{IoContext, JournalError as Error, Result};
use crate::proto;
use loa_core::Patch;
use prost::Message;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Bytes before a record's payload.
pub(crate) const HEADER_LEN: u64 = 8;
/// Largest payload a record may hold. A larger length field is damage.
pub(crate) const MAX_PAYLOAD: u32 = 1 << 28;
/// A sparse offset index entry is kept every this many records.
pub(crate) const CHECKPOINT_EVERY: u64 = 1024;
/// Bytes of the magic and version that open every file.
pub(crate) const PREAMBLE_LEN: usize = 12;

pub(crate) fn le_u32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().expect("4 bytes"))
}

pub(crate) fn le_u64(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"))
}

pub(crate) fn preamble(magic: [u8; 8], version: u32) -> Vec<u8> {
    magic.iter().copied().chain(version.to_le_bytes()).collect()
}

/// Appends one record to `buf`: `payload` writes the payload, then its length
/// and checksum are filled in. Fails with the payload length when too large.
pub(crate) fn put(buf: &mut Vec<u8>, payload: impl FnOnce(&mut Vec<u8>)) -> std::result::Result<(), usize> {
    let start = buf.len();
    buf.extend_from_slice(&[0; HEADER_LEN as usize]);
    payload(buf);
    let body = start + HEADER_LEN as usize;
    let len = buf.len() - body;
    let len32 = u32::try_from(len).ok().filter(|l| *l <= MAX_PAYLOAD).ok_or(len)?;
    let crc = crc32fast::hash(&buf[body..]);
    buf[start..start + 4].copy_from_slice(&len32.to_le_bytes());
    buf[start + 4..body].copy_from_slice(&crc.to_le_bytes());
    Ok(())
}

/// What [`read`] found at a record boundary.
pub(crate) enum Frame {
    /// A record whose payload, now in the buffer, matches its checksum.
    Whole(u32),
    /// The header or the declared payload runs past the room left.
    Short,
    BadLen(u32),
    BadCrc(u32),
}

/// Reads one record that must end within `room` bytes, its payload into `buf`.
pub(crate) fn read(r: &mut impl Read, room: u64, buf: &mut Vec<u8>) -> io::Result<Frame> {
    if room < HEADER_LEN {
        return Ok(Frame::Short);
    }
    let mut h = [0; HEADER_LEN as usize];
    r.read_exact(&mut h)?;
    let len = le_u32(&h, 0);
    if HEADER_LEN + u64::from(len) > room {
        return Ok(Frame::Short);
    }
    if len == 0 || len > MAX_PAYLOAD {
        return Ok(Frame::BadLen(len));
    }
    buf.resize(len as usize, 0);
    r.read_exact(buf)?;
    Ok(if crc32fast::hash(buf) == le_u32(&h, 4) { Frame::Whole(len) } else { Frame::BadCrc(len) })
}

/// A prost message stored as a file header: the preamble, then one record.
pub(crate) trait FramedHeader: Message + Default {
    const MAGIC: [u8; 8];
    const VERSION: u32;
    /// Largest encoded header accepted.
    const MAX_LEN: u32 = 64 * 1024;
}

pub(crate) fn write_framed<H: FramedHeader>(h: &H) -> Vec<u8> {
    let mut bytes = preamble(H::MAGIC, H::VERSION);
    // A header is far below the record limit.
    let _ = put(&mut bytes, |b| b.extend(h.encode_to_vec()));
    bytes
}

/// Reads the header of the file at `path`. Returns it and the offset after it.
pub(crate) fn read_framed<H: FramedHeader>(path: &Path) -> Result<(H, u64)> {
    let mut file = File::open(path).at(path)?;
    let mut fixed = [0u8; PREAMBLE_LEN + HEADER_LEN as usize];
    match file.read_exact(&mut fixed) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(Error::NotAJournal(path.to_owned())),
        Err(e) => return Err(Error::Io { path: path.to_owned(), source: e }),
    }
    if fixed[..8] != H::MAGIC {
        return Err(Error::NotAJournal(path.to_owned()));
    }
    let found = le_u32(&fixed, 8);
    if found != H::VERSION {
        return Err(Error::UnsupportedVersion { path: path.to_owned(), found, supported: H::VERSION });
    }
    let corrupt = || Error::CorruptHeader(path.to_owned());
    let len = le_u32(&fixed, PREAMBLE_LEN);
    if len > H::MAX_LEN {
        return Err(corrupt());
    }
    let mut bytes = vec![0; len as usize];
    file.read_exact(&mut bytes).map_err(|_| corrupt())?;
    if crc32fast::hash(&bytes) != le_u32(&fixed, PREAMBLE_LEN + 4) {
        return Err(corrupt());
    }
    let header = H::decode(bytes.as_slice()).map_err(|_| corrupt())?;
    Ok((header, (fixed.len() + bytes.len()) as u64))
}

/// Encodes `patch` as one framed record into `buf`, replacing its contents.
pub(crate) fn encode(patch: &Patch, buf: &mut Vec<u8>) -> Result<()> {
    buf.clear();
    // Encoding into a Vec never runs out of room.
    put(buf, |b| {
        let _ = proto::Patch::from(patch).encode(b);
    })
    .map_err(Error::TooLarge)
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

    fn read(&mut self) -> Result<Frame> {
        read(&mut self.reader, self.end - self.offset, &mut self.buf).at(&self.path)
    }

    /// Decodes the whole record of `len` bytes just read and steps past it.
    fn accept(&mut self, len: u32) -> Result<Patch> {
        let patch = proto::decode_patch(&self.buf).map_err(|e| self.corrupt(e.to_string()))?;
        if patch.seq != self.next_seq {
            return Err(self.corrupt(format!("expected seq {}, found {}", self.next_seq, patch.seq)));
        }
        self.offset += HEADER_LEN + u64::from(len);
        self.next_seq += 1;
        Ok(patch)
    }

    /// Reads, checks and decodes the next record. `None` at `end`.
    pub(crate) fn next_patch(&mut self) -> Result<Option<Patch>> {
        if self.offset >= self.end {
            return Ok(None);
        }
        match self.read()? {
            Frame::Whole(len) => self.accept(len).map(Some),
            Frame::Short => Err(self.corrupt("record runs past the end")),
            Frame::BadLen(len) => Err(self.corrupt(format!("bad record length {len}"))),
            Frame::BadCrc(_) => Err(self.corrupt("checksum mismatch")),
        }
    }

    /// The next patch, which must be there for the journal to reach `target`.
    pub(crate) fn expect_patch(&mut self, target: u64) -> Result<Patch> {
        self.next_patch()?.ok_or_else(|| self.corrupt(format!("journal ends before seq {target}")))
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
}

/// Checks every record from `start` (which holds `next_seq`) to `file_len`.
///
/// A damaged record is a torn tail when nothing but zeros follows it, or when
/// its declared length runs past the end of the file: those are the shapes an
/// interrupted append leaves. The scan stops before it. Damage with real data
/// after it is [`Error::Corrupt`].
pub(crate) fn scan(path: &Path, file_len: u64, start: u64, next_seq: u64) -> Result<Scan> {
    let mut r = Reader::new(path, start, file_len, next_seq)?;
    let mut out = Scan { end: start, last_seq: next_seq - 1, last_record: None, checkpoints: Vec::new() };
    while r.offset < file_len {
        let off = r.offset;
        // A bad length tells nothing about where the record ends, so only
        // zeros from its start count as a torn write.
        let (tail_from, reason) = match r.read()? {
            Frame::Whole(len) => {
                let patch = r.accept(len)?;
                if (patch.seq - 1).is_multiple_of(CHECKPOINT_EVERY) {
                    out.checkpoints.push((patch.seq, off));
                }
                (out.end, out.last_seq, out.last_record) = (r.offset, patch.seq, Some(off));
                continue;
            }
            Frame::Short => return Ok(out),
            Frame::BadLen(len) => (off, format!("bad record length {len}")),
            Frame::BadCrc(len) => (off + HEADER_LEN + u64::from(len), "checksum mismatch".into()),
        };
        return if zeros_from(path, tail_from)? { Ok(out) } else { Err(r.corrupt(reason)) };
    }
    Ok(out)
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
    let mut buf = Vec::new();
    let Frame::Whole(len) = read(&mut file, file_len - offset, &mut buf).at(path)? else { return Ok(None) };
    Ok(proto::decode_patch(&buf).ok().map(|p| (p.seq, offset + HEADER_LEN + u64::from(len))))
}
