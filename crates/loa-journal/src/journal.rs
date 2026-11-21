//! The on-disk journal of one session.
//!
//! A session lives in its own directory:
//!
//! - `journal.loaj`: header, then one framed record per patch (see [`crate::record`])
//! - `snap-<seq>.loas`: DOM snapshots, written atomically every N patches
//! - `lock`: held while a [`Journal`] has the session open, so it has one writer
//!
//! Header: `LOAJRNL\0`, format version (u32 LE), header length (u32 LE),
//! header crc32 (u32 LE), then the prost `Header` message with the session id
//! and, for forks, the parent session id and fork sequence.

use crate::error::{Error, IoContext, Result};
use crate::record::{self, CHECKPOINT_EVERY, Reader};
use crate::snapshot::{self, sync_dir};
use loa_core::{Dom, Draft, Patch, Value, attr, now_ms};
use prost::Message as _;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// File name of the journal inside a session directory.
pub const JOURNAL_FILE: &str = "journal.loaj";
const LOCK_FILE: &str = "lock";
const MAGIC: [u8; 8] = *b"LOAJRNL\0";
/// Version of the journal file layout. Bumped with a migration.
pub const FORMAT_VERSION: u32 = 1;
const FIXED_HEADER: usize = 20;
const MAX_HEADER: u32 = 64 * 1024;

#[derive(Clone, PartialEq, prost::Message)]
struct Header {
    #[prost(string, tag = "1")]
    session_id: String,
    #[prost(string, optional, tag = "2")]
    parent_session: Option<String>,
    #[prost(uint64, optional, tag = "3")]
    forked_at: Option<u64>,
}

/// The session a fork was made from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parent {
    pub session_id: String,
    /// Sequence of the source's last patch that the fork shares.
    pub forked_at: u64,
}

/// Tuning of a journal.
#[derive(Clone, Debug)]
pub struct JournalOptions {
    /// Write a snapshot after this many patches since the last one. 0 never
    /// snapshots on its own.
    pub snapshot_every: u64,
    /// Snapshots kept on disk; older ones are deleted. At least 1 is kept.
    /// More than 1 lets a load fall back when the newest is damaged.
    pub keep_snapshots: usize,
}

impl Default for JournalOptions {
    fn default() -> Self {
        Self { snapshot_every: 1000, keep_snapshots: 2 }
    }
}

/// Bytes cut from the end of a journal on open because the last append was
/// interrupted (a crash or power loss mid-write).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Truncation {
    /// New length of the journal file.
    pub offset: u64,
    pub dropped_bytes: u64,
    /// Sequence of the last patch that survived.
    pub last_seq: u64,
}

/// What a session directory holds, read without opening it for writing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub parent: Option<Parent>,
    /// Sequence of the last whole patch. A torn tail is not counted.
    pub last_seq: u64,
}

/// An open session journal. Holds the session's lock until dropped.
///
/// Appends go straight to the file (one write per patch) so a process crash
/// loses nothing; [`Journal::sync`] makes them survive power loss.
pub struct Journal {
    dir: PathBuf,
    path: PathBuf,
    file: File,
    _lock: File,
    session_id: String,
    parent: Option<Parent>,
    options: JournalOptions,
    /// Offset of the first record.
    data_start: u64,
    /// Offset after the last record, the file length.
    end: u64,
    last_seq: u64,
    /// Offset of the record at `last_seq`.
    last_record: Option<u64>,
    /// Sparse index: record sequence to its offset. Always holds `(1, data_start)`.
    checkpoints: BTreeMap<u64, u64>,
    /// Snapshots whose header and journal anchor check out, oldest first.
    snapshots: Vec<snapshot::Meta>,
    /// Appends not yet fsynced.
    dirty: bool,
    /// A failed write left bytes that could not be cut off again.
    damaged: bool,
    buf: Vec<u8>,
}

impl Journal {
    /// Creates the session directory `dir` with an empty journal for `session_id`.
    pub fn create(dir: &Path, session_id: &str, options: JournalOptions) -> Result<Self> {
        Self::create_with(dir, session_id, None, options)
    }

    fn create_with(dir: &Path, session_id: &str, parent: Option<Parent>, options: JournalOptions) -> Result<Self> {
        validate_session_id(session_id)?;
        match fs::create_dir(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::Exists(session_id.to_owned()));
            }
            Err(e) => return Err(Error::Io { path: dir.to_owned(), source: e }),
        }
        let lock = lock(dir)?;
        let path = dir.join(JOURNAL_FILE);
        let header = Header {
            session_id: session_id.to_owned(),
            parent_session: parent.as_ref().map(|p| p.session_id.clone()),
            forked_at: parent.as_ref().map(|p| p.forked_at),
        };
        let mut bytes = header.encode_to_vec();
        let len = bytes.len() as u32;
        let crc = crc32fast::hash(&bytes);
        bytes.splice(
            0..0,
            MAGIC.iter().copied().chain(FORMAT_VERSION.to_le_bytes()).chain(len.to_le_bytes()).chain(crc.to_le_bytes()),
        );
        let mut file = OpenOptions::new().read(true).append(true).create_new(true).open(&path).at(&path)?;
        file.write_all(&bytes).at(&path)?;
        file.sync_all().at(&path)?;
        sync_dir(dir)?;
        if let Some(root) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            sync_dir(root)?;
        }
        let data_start = bytes.len() as u64;
        Ok(Self {
            dir: dir.to_owned(),
            path,
            file,
            _lock: lock,
            session_id: session_id.to_owned(),
            parent,
            options,
            data_start,
            end: data_start,
            last_seq: 0,
            last_record: None,
            checkpoints: BTreeMap::from([(1, data_start)]),
            snapshots: Vec::new(),
            dirty: false,
            damaged: false,
            buf: Vec::new(),
        })
    }

    /// Opens the session in `dir` for writing.
    ///
    /// Only records after the newest valid snapshot are read. A torn last
    /// record is cut off and reported as a [`Truncation`]; damage followed by
    /// more records is [`Error::Corrupt`].
    pub fn open(dir: &Path, options: JournalOptions) -> Result<(Self, Option<Truncation>)> {
        let lock = lock(dir)?;
        let found = recover(dir)?;
        let path = dir.join(JOURNAL_FILE);
        let truncation = if found.end < found.file_len {
            truncate_file(&path, found.end).at(&path)?;
            Some(Truncation { offset: found.end, dropped_bytes: found.file_len - found.end, last_seq: found.last_seq })
        } else {
            None
        };
        let file = OpenOptions::new().read(true).append(true).open(&path).at(&path)?;
        remove_temp_files(dir)?;
        let journal = Self {
            dir: dir.to_owned(),
            path,
            file,
            _lock: lock,
            session_id: found.session_id,
            parent: found.parent,
            options,
            data_start: found.data_start,
            end: found.end,
            last_seq: found.last_seq,
            last_record: found.last_record,
            checkpoints: found.checkpoints,
            snapshots: found.snapshots,
            dirty: false,
            damaged: false,
            buf: Vec::new(),
        };
        Ok((journal, truncation))
    }

    /// Reads the id, fork link and last sequence of the session in `dir`
    /// without locking or repairing it.
    pub fn inspect(dir: &Path) -> Result<SessionInfo> {
        let found = recover(dir)?;
        Ok(SessionInfo { id: found.session_id, parent: found.parent, last_seq: found.last_seq })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The session this one was forked from.
    pub fn parent(&self) -> Option<&Parent> {
        self.parent.as_ref()
    }

    /// Sequence of the last appended patch, 0 when empty.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// The session directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Sequences of the snapshots on disk, oldest first.
    pub fn snapshot_seqs(&self) -> impl Iterator<Item = u64> + '_ {
        self.snapshots.iter().map(|m| m.seq)
    }

    /// True when appended patches have not been fsynced yet.
    pub fn has_unsynced(&self) -> bool {
        self.dirty
    }

    /// Appends the next patch. Its sequence must follow [`Journal::last_seq`].
    /// The bytes reach the OS before this returns; call [`Journal::sync`] for
    /// durability.
    pub fn append(&mut self, patch: &Patch) -> Result<()> {
        if self.damaged {
            return Err(Error::Corrupt {
                path: self.path.clone(),
                offset: self.end,
                reason: "an earlier failed write left a partial record; reopen the journal".into(),
            });
        }
        if patch.seq != self.last_seq + 1 {
            return Err(Error::SequenceGap { last: self.last_seq, got: patch.seq });
        }
        record::encode(patch, &mut self.buf)?;
        if let Err(source) = self.file.write_all(&self.buf) {
            // Cut a partial record so the file ends on a record boundary. If
            // that fails too, refuse further appends: they would land after
            // the partial bytes and turn a torn tail into mid-file damage.
            self.damaged = truncate_file(&self.path, self.end).is_err();
            return Err(Error::Io { path: self.path.clone(), source });
        }
        if (patch.seq - 1).is_multiple_of(CHECKPOINT_EVERY) {
            self.checkpoints.insert(patch.seq, self.end);
        }
        self.last_record = Some(self.end);
        self.end += self.buf.len() as u64;
        self.last_seq = patch.seq;
        self.dirty = true;
        Ok(())
    }

    /// Appends a patch the caller has already applied to `dom`, then writes a
    /// snapshot of `dom` when one is due.
    pub fn append_applied(&mut self, patch: &Patch, dom: &Dom) -> Result<()> {
        if dom.seq() != patch.seq {
            return Err(Error::DomMismatch { dom: dom.seq(), journal: patch.seq });
        }
        self.append(patch)?;
        if self.snapshot_due() {
            self.write_snapshot(dom)?;
        }
        Ok(())
    }

    /// True when [`JournalOptions::snapshot_every`] patches followed the last snapshot.
    pub fn snapshot_due(&self) -> bool {
        let every = self.options.snapshot_every;
        let last = self.snapshots.last().map_or(0, |m| m.seq);
        every > 0 && self.last_seq - last >= every
    }

    /// Fsyncs appended patches.
    pub fn sync(&mut self) -> Result<()> {
        if self.dirty {
            self.file.sync_data().at(&self.path)?;
            self.dirty = false;
        }
        Ok(())
    }

    /// Writes `dom`, which must be at [`Journal::last_seq`], as a snapshot.
    /// Syncs the journal first so a snapshot never covers patches that could
    /// still be lost.
    pub fn write_snapshot(&mut self, dom: &Dom) -> Result<()> {
        let mismatch = Error::DomMismatch { dom: dom.seq(), journal: self.last_seq };
        // `last_record` is set whenever `last_seq` is above 0.
        let Some(prev) = self.last_record.filter(|_| dom.seq() == self.last_seq) else {
            return Err(mismatch);
        };
        if self.snapshots.last().is_some_and(|m| m.seq == self.last_seq) {
            return Ok(());
        }
        self.sync()?;
        let meta = snapshot::write(&self.dir, dom, self.end, prev)?;
        self.checkpoints.insert(meta.seq + 1, meta.offset);
        self.snapshots.push(meta);
        let keep = self.options.keep_snapshots.max(1);
        while self.snapshots.len() > keep {
            let old = self.snapshots.remove(0);
            match fs::remove_file(&old.path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(Error::Io { path: old.path, source: e }),
            }
        }
        Ok(())
    }

    /// Patches from `from_seq` (1 when 0) to the current end, read from disk.
    /// Later appends are not included. Peers replicate from any position with it.
    pub fn patches(&self, from_seq: u64) -> Result<Patches> {
        let from = from_seq.max(1);
        if from > self.last_seq + 1 {
            return Err(Error::OutOfRange { seq: from, last: self.last_seq });
        }
        let offset = self.offset_of(from)?;
        Ok(Patches { reader: Some(Reader::new(&self.path, offset, self.end, from)?) })
    }

    /// The DOM after the last patch: the newest valid snapshot plus the
    /// patches after it. A damaged snapshot falls back to the one before it,
    /// and finally to a replay from the first patch.
    pub fn load(&self) -> Result<Dom> {
        self.load_at(self.last_seq)
    }

    /// The DOM right after patch `seq` (the empty DOM for 0).
    pub fn load_at(&self, seq: u64) -> Result<Dom> {
        if seq > self.last_seq {
            return Err(Error::OutOfRange { seq, last: self.last_seq });
        }
        for meta in self.snapshots.iter().rev().filter(|m| m.seq <= seq) {
            if let Ok(dom) = snapshot::read_dom(meta).and_then(|dom| self.replay(dom, meta.offset, seq)) {
                return Ok(dom);
            }
        }
        self.replay(Dom::new(), self.data_start, seq)
    }

    /// Applies records from `offset` (which holds `dom.seq() + 1`) up to `target`.
    fn replay(&self, mut dom: Dom, offset: u64, target: u64) -> Result<Dom> {
        let mut reader = Reader::new(&self.path, offset, self.end, dom.seq() + 1)?;
        while dom.seq() < target {
            let patch = reader.next_patch()?.ok_or_else(|| Error::Corrupt {
                path: self.path.clone(),
                offset: reader.offset(),
                reason: format!("journal ends before seq {target}"),
            })?;
            dom.apply(&patch).map_err(|source| Error::Apply { seq: patch.seq, source })?;
        }
        Ok(dom)
    }

    /// Offset of the record at `seq`, or the end offset for `last_seq + 1`.
    fn offset_of(&self, seq: u64) -> Result<u64> {
        if seq == self.last_seq + 1 {
            return Ok(self.end);
        }
        if seq == self.last_seq
            && let Some(offset) = self.last_record
        {
            return Ok(offset);
        }
        let (&from_seq, &from) = self
            .checkpoints
            .range(..=seq)
            .next_back()
            .ok_or(Error::OutOfRange { seq, last: self.last_seq })?;
        let mut reader = Reader::new(&self.path, from, self.end, from_seq)?;
        while reader.next_seq() < seq {
            reader.skip()?;
        }
        Ok(reader.offset())
    }

    /// Forks the session after patch `at_seq` into a sibling directory named
    /// `new_session_id`.
    ///
    /// The fork holds a copy of patches `1..=at_seq`, so its DOM at `at_seq`
    /// equals this one's and it stays loadable when the source is deleted.
    /// Its header links the source. When the DOM has a root, the fork's first
    /// own patches set the root's `id`, `parent_session` and `forked_at`.
    /// The source is only read.
    pub fn fork(&self, at_seq: u64, new_session_id: &str) -> Result<Journal> {
        validate_session_id(new_session_id)?;
        let dom = self.load_at(at_seq)?;
        let cut = self.offset_of(at_seq + 1)?;
        let at_record = if at_seq > 0 { Some(self.offset_of(at_seq)?) } else { None };
        let dir = self.dir.parent().unwrap_or(Path::new("")).join(new_session_id);
        let parent = Parent { session_id: self.session_id.clone(), forked_at: at_seq };
        let fork = Self::create_with(&dir, new_session_id, Some(parent), self.options.clone())?;
        let filled = self.fill_fork(fork, dom, cut, at_record);
        if filled.is_err() {
            // Leave no half made session behind. The fork was dropped above.
            let _ = fs::remove_dir_all(&dir);
        }
        filled
    }

    fn fill_fork(&self, mut fork: Journal, mut dom: Dom, cut: u64, at_record: Option<u64>) -> Result<Journal> {
        let len = cut - self.data_start;
        let mut src = File::open(&self.path).at(&self.path)?;
        src.seek(SeekFrom::Start(self.data_start)).at(&self.path)?;
        let copied = std::io::copy(&mut src.take(len), &mut fork.file).at(&fork.path)?;
        if copied != len {
            return Err(Error::Corrupt {
                path: self.path.clone(),
                offset: self.data_start + copied,
                reason: "journal shrank while forking".into(),
            });
        }
        let shift = |offset: u64| offset - self.data_start + fork.data_start;
        let at_seq = dom.seq();
        for (&seq, &offset) in self.checkpoints.range(..=at_seq) {
            fork.checkpoints.insert(seq, shift(offset));
        }
        fork.end = shift(cut);
        fork.last_seq = at_seq;
        fork.last_record = at_record.map(shift);
        fork.dirty = true;
        if let Some(root) = dom.root().map(|n| n.id) {
            let drafts = [
                Draft::set(root, attr::ID, fork.session_id.as_str()),
                Draft::set(root, attr::PARENT_SESSION, self.session_id.as_str()),
                Draft::set(root, attr::FORKED_AT, Value::Int(i64::try_from(at_seq).unwrap_or(i64::MAX))),
            ];
            let ts = now_ms();
            for draft in drafts {
                let patch = dom.commit(draft, ts).map_err(|source| Error::Apply { seq: dom.seq() + 1, source })?;
                fork.append(&patch)?;
            }
        }
        if fork.last_seq > 0 {
            fork.write_snapshot(&dom)?;
        } else {
            fork.sync()?;
        }
        Ok(fork)
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        // Best effort: drop can't report errors. Callers that need to know
        // call `sync` first.
        if self.dirty {
            let _ = self.file.sync_data();
        }
    }
}

/// Patches read from a journal in order. Stops after the first error.
pub struct Patches {
    reader: Option<Reader>,
}

impl Iterator for Patches {
    type Item = Result<Patch>;

    fn next(&mut self) -> Option<Result<Patch>> {
        let reader = self.reader.as_mut()?;
        match reader.next_patch() {
            Ok(Some(patch)) => Some(Ok(patch)),
            Ok(None) => {
                self.reader = None;
                None
            }
            Err(e) => {
                self.reader = None;
                Some(Err(e))
            }
        }
    }
}

/// Checks that `id` is safe as a directory name on every platform.
pub(crate) fn validate_session_id(id: &str) -> Result<()> {
    let ok = (1..=128).contains(&id.len())
        && !id.starts_with('.')
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    if ok { Ok(()) } else { Err(Error::InvalidSessionId(id.to_owned())) }
}

/// How long [`lock`] keeps trying a held lock before reporting it locked.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// Takes the session's exclusive lock.
///
/// A process the app forks gets a copy of every open lock file until it
/// execs, which closes them (they are close-on-exec). A lock found held is
/// tried again for [`LOCK_WAIT`] so such a moment doesn't read as another
/// writer; a lock really held by another writer stays held past it.
pub(crate) fn lock(dir: &Path) -> Result<File> {
    let path = dir.join(LOCK_FILE);
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(&path).at(&path)?;
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(std::fs::TryLockError::WouldBlock) => return Err(Error::Locked(dir.to_owned())),
            Err(std::fs::TryLockError::Error(e)) => return Err(Error::Io { path, source: e }),
        }
    }
}

/// Truncate `path` to `len` through a dedicated write handle: an append-only
/// handle lacks the write-data access `set_len` needs on Windows.
fn truncate_file(path: &Path, len: u64) -> std::io::Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(len)?;
    file.sync_all()
}

fn remove_temp_files(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir).at(dir)? {
        let entry = entry.at(dir)?;
        if entry.file_name().to_str().is_some_and(|n| n.ends_with(".tmp")) {
            fs::remove_file(entry.path()).at(entry.path())?;
        }
    }
    Ok(())
}

/// State of a journal file as found on disk.
struct Recovered {
    session_id: String,
    parent: Option<Parent>,
    data_start: u64,
    file_len: u64,
    end: u64,
    last_seq: u64,
    last_record: Option<u64>,
    checkpoints: BTreeMap<u64, u64>,
    snapshots: Vec<snapshot::Meta>,
}

fn recover(dir: &Path) -> Result<Recovered> {
    let path = dir.join(JOURNAL_FILE);
    let (header, data_start) = read_header(&path)?;
    let file_len = fs::metadata(&path).at(&path)?.len();
    let mut snapshots = Vec::new();
    for meta in snapshot::list(dir)? {
        // A snapshot counts only when the record it says it follows is
        // really there and ends where the snapshot resumes.
        if meta.prev_offset >= data_start && record::probe(&path, meta.prev_offset, file_len)? == Some((meta.seq, meta.offset))
        {
            snapshots.push(meta);
        }
    }
    let (start, next_seq) = snapshots.last().map_or((data_start, 1), |m| (m.offset, m.seq + 1));
    let scan = record::scan(&path, file_len, start, next_seq)?;
    let mut checkpoints = BTreeMap::from([(1, data_start)]);
    checkpoints.extend(snapshots.iter().map(|m| (m.seq + 1, m.offset)));
    checkpoints.extend(scan.checkpoints);
    let parent = match (header.parent_session, header.forked_at) {
        (Some(session_id), Some(forked_at)) => Some(Parent { session_id, forked_at }),
        (None, None) => None,
        _ => return Err(Error::CorruptHeader(path)),
    };
    Ok(Recovered {
        session_id: header.session_id,
        parent,
        data_start,
        file_len,
        end: scan.end,
        last_seq: scan.last_seq,
        last_record: scan.last_record.or_else(|| snapshots.last().map(|m| m.prev_offset)),
        checkpoints,
        snapshots,
    })
}

fn read_header(path: &Path) -> Result<(Header, u64)> {
    let mut file = File::open(path).at(path)?;
    let mut fixed = [0u8; FIXED_HEADER];
    match file.read_exact(&mut fixed) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(Error::NotAJournal(path.to_owned())),
        Err(e) => return Err(Error::Io { path: path.to_owned(), source: e }),
    }
    if fixed[..8] != MAGIC {
        return Err(Error::NotAJournal(path.to_owned()));
    }
    let u32_at = |i: usize| u32::from_le_bytes([fixed[i], fixed[i + 1], fixed[i + 2], fixed[i + 3]]);
    let version = u32_at(8);
    if version != FORMAT_VERSION {
        return Err(Error::UnsupportedVersion { path: path.to_owned(), found: version, supported: FORMAT_VERSION });
    }
    let len = u32_at(12);
    if len > MAX_HEADER {
        return Err(Error::CorruptHeader(path.to_owned()));
    }
    let mut bytes = vec![0; len as usize];
    file.read_exact(&mut bytes).map_err(|_| Error::CorruptHeader(path.to_owned()))?;
    if crc32fast::hash(&bytes) != u32_at(16) {
        return Err(Error::CorruptHeader(path.to_owned()));
    }
    let header = Header::decode(bytes.as_slice()).map_err(|_| Error::CorruptHeader(path.to_owned()))?;
    Ok((header, (FIXED_HEADER + bytes.len()) as u64))
}
