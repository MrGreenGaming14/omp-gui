use std::io;
use std::path::{Path, PathBuf};

use loa_util::fs::{self, Snapshot};

use crate::rpc::RpcError;

/// Errors from reading, editing and validating model settings.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file could not be read or written.
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A document did not parse (YAML, TOML or JSON).
    #[error("{what} does not parse: {message}")]
    Parse { what: String, message: String },
    /// The document uses a layout the in place editor does not edit (flow style
    /// root, several documents, a non mapping root).
    #[error("cannot edit {what}: {reason}")]
    Unsupported { what: String, reason: String },
    /// A text edit did not produce the intended document; nothing was changed.
    #[error("editing {path} would change more than the edited key; nothing was written")]
    EditCheck { path: String },
    /// The file changed on disk after it was loaded; reload and edit again.
    #[error("{path} changed on disk since it was loaded")]
    ChangedOnDisk { path: PathBuf },
    /// A value given to an edit is not one omp or the harness accepts.
    #[error("invalid value for {field}: {reason}")]
    Invalid { field: String, reason: String },
    /// omp itself rejected the edited file.
    #[error("omp rejected {what}: {message}")]
    Rejected { what: String, message: String },
    /// An external program (omp, claude, codex) could not be run.
    #[error("cannot run {program}: {message}")]
    Process { program: String, message: String },
    /// An omp RPC call failed.
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn invalid(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Invalid {
            field: field.into(),
            reason: reason.into(),
        }
    }
}

/// Result with this crate's [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// `bytes` read from `path` as UTF-8 text.
fn text(path: &Path, bytes: Option<&[u8]>) -> Result<Option<String>> {
    let utf8 = |b: &[u8]| String::from_utf8(b.to_vec()).map_err(|e| Error::io(path, io::Error::new(io::ErrorKind::InvalidData, e)));
    bytes.map(utf8).transpose()
}

/// Reads `path` as text, `None` when it does not exist.
pub(crate) fn read_optional(path: &Path) -> Result<Option<String>> {
    text(path, fs::read_optional(path).map_err(|e| Error::io(path, e))?.as_deref())
}

/// Loads `path` and its text, remembering what was seen for [`save_snapshot`].
pub(crate) fn load_snapshot(path: &Path) -> Result<(Snapshot, Option<String>)> {
    let snapshot = Snapshot::load(path).map_err(|e| Error::io(path, e))?;
    let text = text(path, snapshot.contents())?;
    Ok((snapshot, text))
}

/// Writes `text` atomically unless the file changed since it was loaded.
pub(crate) fn save_snapshot(snapshot: &mut Snapshot, text: &str) -> Result<()> {
    let path = snapshot.path().to_path_buf();
    match snapshot.save(text.as_bytes()) {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::ChangedOnDisk { path }),
        Err(e) => Err(Error::io(path, e)),
    }
}
