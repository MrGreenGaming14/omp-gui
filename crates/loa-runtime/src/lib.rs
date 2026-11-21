//! The headless app runtime: the session library, the live sessions, the
//! harnesses they run on and the active project. No UI.

pub mod feed;
pub mod harnesses;
pub mod lifecycle;
pub mod session;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use loa_bus::{SessionId, SessionWriter};
use loa_session::library::Library;

use crate::harnesses::Harnesses;
use crate::session::{Session, Sessions};

/// What a [`Runtime`] starts from.
pub struct RuntimeParts {
    /// Session journals, opened as writers on one bus.
    pub library: Arc<Library>,
    pub harnesses: Harnesses,
    /// Public key of this node, written to `<session node>`.
    pub node: String,
    /// Directory new sessions run in.
    pub cwd: PathBuf,
    /// The active project, which new sessions join; `None` for no project.
    pub project: Option<String>,
    /// Approval mode new sessions run in; `None` for the harness default.
    pub access: Option<String>,
}

/// The sessions, their library and harnesses, and the active project.
pub struct Runtime {
    library: Arc<Library>,
    harnesses: Harnesses,
    sessions: Sessions,
    node: String,
    cwd: PathBuf,
    project: Option<String>,
    access: Option<String>,
    /// The approval mode changed while a turn ran: the active session
    /// restarts on it once the turn ends.
    restart_for_access: bool,
    /// Starts, resumes and spawns in flight.
    operations: lifecycle::Operations,
}

impl Runtime {
    pub fn new(parts: RuntimeParts) -> Self {
        let RuntimeParts { library, harnesses, node, cwd, project, access } = parts;
        Self {
            library,
            harnesses,
            sessions: Sessions::default(),
            node,
            cwd,
            project,
            access,
            restart_for_access: false,
            operations: lifecycle::Operations::default(),
        }
    }

    pub fn library(&self) -> &Arc<Library> {
        &self.library
    }

    pub fn harnesses(&self) -> &Harnesses {
        &self.harnesses
    }

    pub fn harnesses_mut(&mut self) -> &mut Harnesses {
        &mut self.harnesses
    }

    pub fn sessions(&self) -> &Sessions {
        &self.sessions
    }

    pub fn sessions_mut(&mut self) -> &mut Sessions {
        &mut self.sessions
    }

    /// Public key of this node.
    pub fn node(&self) -> &str {
        &self.node
    }

    /// Directory new sessions run in.
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// The active project, which new sessions join; `None` for no project.
    /// Projects group sessions; they are not folders.
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// Makes `project` the active one, and `cwd`, when given, the folder new sessions run in.
    pub fn set_active(&mut self, project: String, cwd: Option<PathBuf>) {
        self.project = Some(project);
        if let Some(cwd) = cwd {
            self.cwd = cwd;
        }
    }

    /// Makes `cwd` the folder new sessions run in; the active project stays.
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
    }

    /// No project is active: new sessions join none.
    pub fn clear_project(&mut self) {
        self.project = None;
    }

    /// Approval mode new sessions run in; `None` for the harness default.
    pub fn access(&self) -> Option<&str> {
        self.access.as_deref()
    }

    pub fn set_access(&mut self, access: Option<String>) {
        self.access = access;
    }

    /// Whether the active session restarts on the approval mode once its turn ends.
    pub fn restart_for_access(&self) -> bool {
        self.restart_for_access
    }

    pub fn set_restart_for_access(&mut self, restart: bool) {
        self.restart_for_access = restart;
    }

    /// Creates a new session's journal and writer.
    pub fn create_session(&mut self) -> Result<(SessionId, Arc<SessionWriter>), String> {
        self.sessions.create(&self.library)
    }

    /// Opens a session from its journal unless it is open already.
    pub fn load_session(&mut self, id: &str) -> Result<(), String> {
        self.sessions.load(&self.library, id)
    }

    /// Closes a session's writer and releases its journal, deleting the
    /// journal too when `delete` is set.
    pub fn close_session(&mut self, id: &str, delete: bool) -> (Option<Session>, Option<String>) {
        self.sessions.close(&self.library, id, delete)
    }

    /// Deletes the journals of the empty sessions [`session::recents`] found, unless they
    /// were opened since; returns a note per one that couldn't be deleted.
    pub fn delete_empty(&self, empty: Vec<String>) -> Vec<String> {
        let closed = empty.into_iter().filter(|id| self.sessions.get(id).is_none());
        closed.filter_map(|id| self.library.delete(&id).err().map(|error| format!("Can't delete empty session {id}: {error}"))).collect()
    }
}
