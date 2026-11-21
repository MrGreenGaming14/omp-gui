use crate::proto::ProtoError;
use loa_core::ApplyError;
use std::path::PathBuf;

/// Everything that can go wrong with a journal, its snapshots or the store.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} is not a journal")]
    NotAJournal(PathBuf),
    #[error("{path} uses journal format {found}, this build reads {supported}")]
    UnsupportedVersion { path: PathBuf, found: u32, supported: u32 },
    #[error("{0}: header is corrupt")]
    CorruptHeader(PathBuf),
    /// A damaged record that is followed by more data, so it isn't a torn write.
    #[error("{path}: corrupt record at byte {offset}: {reason}")]
    Corrupt { path: PathBuf, offset: u64, reason: String },
    #[error("patch seq {got} doesn't follow seq {last}")]
    SequenceGap { last: u64, got: u64 },
    #[error("seq {seq} is past the last patch {last}")]
    OutOfRange { seq: u64, last: u64 },
    #[error("encoded patch takes {0} bytes, more than a record holds")]
    TooLarge(usize),
    #[error("the DOM is at seq {dom}, the journal at seq {journal}")]
    DomMismatch { dom: u64, journal: u64 },
    #[error("replaying patch {seq}: {source}")]
    Apply {
        seq: u64,
        #[source]
        source: ApplyError,
    },
    #[error("session id {0:?} must be 1 to 128 ASCII letters, digits, '-', '_' or '.', and not start with '.'")]
    InvalidSessionId(String),
    #[error("session {0} already exists")]
    Exists(String),
    #[error("session {0} doesn't exist")]
    NotFound(String),
    #[error("session at {0} is open in another journal")]
    Locked(PathBuf),
    /// The journal writer was closed and released its journal.
    #[error("the session's journal was closed")]
    Closed,
    #[error(transparent)]
    Proto(#[from] ProtoError),
}

pub(crate) type Result<T, E = JournalError> = std::result::Result<T, E>;

/// Attaches a path to io errors.
pub(crate) trait IoContext<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T>;
}

impl<T> IoContext<T> for std::io::Result<T> {
    fn at(self, path: impl Into<PathBuf>) -> Result<T> {
        self.map_err(|source| JournalError::Io { path: path.into(), source })
    }
}
