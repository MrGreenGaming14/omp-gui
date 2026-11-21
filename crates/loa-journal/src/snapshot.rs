//! Snapshot files: the whole DOM at one sequence, beside the journal.
//!
//! Layout: a fixed 52 byte header (magic, version, seq, journal offset of the
//! next record, offset of the record at `seq`, body length, body crc32,
//! header crc32) followed by a `proto::Dom` body. The header alone tells the
//! journal where to resume, so opening reads no DOM.

use crate::error::{Error, IoContext, Result};
use crate::proto;
use loa_core::Dom;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

const MAGIC: [u8; 8] = *b"LOASNAP\0";
const VERSION: u32 = 1;
const HEADER_LEN: usize = 52;
const PREFIX: &str = "snap-";
const SUFFIX: &str = ".loas";

/// Header of one snapshot file whose header checksum holds.
#[derive(Clone, Debug)]
pub(crate) struct Meta {
    pub path: PathBuf,
    pub seq: u64,
    /// Journal offset of the record after `seq`.
    pub offset: u64,
    /// Journal offset of the record at `seq`.
    pub prev_offset: u64,
    body_len: u64,
    body_crc: u32,
}

fn file_name(seq: u64) -> String {
    format!("{PREFIX}{seq:020}{SUFFIX}")
}

/// Every snapshot in `dir` with a valid header, oldest first. Files that are
/// not snapshots, half written temp files and damaged headers are skipped.
pub(crate) fn list(dir: &Path) -> Result<Vec<Meta>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).at(dir)? {
        let entry = entry.at(dir)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(seq) = name.strip_prefix(PREFIX).and_then(|s| s.strip_suffix(SUFFIX)) else { continue };
        if seq.parse::<u64>().is_err() {
            continue;
        }
        if let Some(meta) = read_header(&entry.path())? {
            out.push(meta);
        }
    }
    out.sort_by_key(|m| m.seq);
    Ok(out)
}

fn read_header(path: &Path) -> Result<Option<Meta>> {
    let mut h = [0u8; HEADER_LEN];
    let mut file = File::open(path).at(path)?;
    match file.read_exact(&mut h) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(Error::Io { path: path.to_owned(), source: e }),
    }
    let u32_at = |i: usize| u32::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3]]);
    let u64_at = |i: usize| u64::from_le_bytes([h[i], h[i + 1], h[i + 2], h[i + 3], h[i + 4], h[i + 5], h[i + 6], h[i + 7]]);
    if h[..8] != MAGIC || u32_at(48) != crc32fast::hash(&h[..48]) || u32_at(8) != VERSION {
        return Ok(None);
    }
    let meta = Meta {
        path: path.to_owned(),
        seq: u64_at(12),
        offset: u64_at(20),
        prev_offset: u64_at(28),
        body_len: u64_at(36),
        body_crc: u32_at(44),
    };
    Ok((meta.seq > 0 && meta.prev_offset < meta.offset).then_some(meta))
}

/// Reads and checks the DOM of `meta`.
pub(crate) fn read_dom(meta: &Meta) -> Result<Dom> {
    let path = &meta.path;
    let mut file = File::open(path).at(path)?;
    let len = file.metadata().at(path)?.len();
    if len != HEADER_LEN as u64 + meta.body_len {
        return Err(corrupt(path, "body length doesn't match the file"));
    }
    let mut body = Vec::with_capacity(meta.body_len as usize);
    file.read_to_end(&mut body).at(path)?;
    let body = body.get(HEADER_LEN..).ok_or_else(|| corrupt(path, "file shorter than its header"))?;
    if crc32fast::hash(body) != meta.body_crc {
        return Err(corrupt(path, "body checksum mismatch"));
    }
    let dom = proto::decode_dom(body)?;
    if dom.seq() != meta.seq {
        return Err(corrupt(path, "body seq differs from header"));
    }
    Ok(dom)
}

fn corrupt(path: &Path, reason: &str) -> Error {
    Error::Corrupt { path: path.to_owned(), offset: 0, reason: reason.to_owned() }
}

/// Writes `dom` as the snapshot at `dom.seq()`: temp file, fsync, rename,
/// fsync of the directory. A crash leaves either no snapshot or a whole one.
pub(crate) fn write(dir: &Path, dom: &Dom, offset: u64, prev_offset: u64) -> Result<Meta> {
    let mut buf = vec![0u8; HEADER_LEN];
    proto::encode_dom(dom, &mut buf);
    let body = &buf[HEADER_LEN..];
    let body_len = body.len() as u64;
    let body_crc = crc32fast::hash(body);
    let seq = dom.seq();
    let h = &mut buf[..HEADER_LEN];
    h[..8].copy_from_slice(&MAGIC);
    h[8..12].copy_from_slice(&VERSION.to_le_bytes());
    h[12..20].copy_from_slice(&seq.to_le_bytes());
    h[20..28].copy_from_slice(&offset.to_le_bytes());
    h[28..36].copy_from_slice(&prev_offset.to_le_bytes());
    h[36..44].copy_from_slice(&body_len.to_le_bytes());
    h[44..48].copy_from_slice(&body_crc.to_le_bytes());
    let header_crc = crc32fast::hash(&h[..48]);
    h[48..52].copy_from_slice(&header_crc.to_le_bytes());

    let path = dir.join(file_name(seq));
    loa_util::fs::write_atomic(&path, &buf, None).at(&path)?;
    Ok(Meta { path, seq, offset, prev_offset, body_len, body_crc })
}
