//! Long-lived services created once at startup and shared by every area: the
//! node identity, data directories, the session library (journal store and
//! bus) and the harness registry.

use loa_util::RwLockExt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use loa_bus::{Authorize, Bus, Denied, NodeKey, SessionId};
use loa_config::ConfigStore;
use loa_core::{Dom, Draft};
use loa_features::library::Library;
use loa_identity::{KeySource, NodeIdentity, load_app_identity};
use loa_bus::{JournalOptions, Store};

mod harness;
mod host;

pub use harness::{CHECKING, Harness, Harnesses, OPENCODE_URL};
pub use host::hostname;

/// This node: its ed25519 identity and the label people see.
pub struct Node {
    pub identity: NodeIdentity,
    /// Public key as text, written to `<session node>` and used as the
    /// node's key in the sidebar.
    pub key: String,
    /// Human name, the machine's host name.
    pub label: String,
    /// Where the key is kept between runs.
    pub key_source: KeySource,
}

impl Node {
    /// A node over a key handed in, kept nowhere (tests).
    pub fn new(identity: NodeIdentity, label: String) -> Self {
        Self::stored(identity, label, KeySource::Memory { reason: "the key was handed in".into() })
    }

    /// A node over a key loaded from `key_source`.
    pub fn stored(identity: NodeIdentity, label: String, key_source: KeySource) -> Self {
        let key = identity.public_key().to_endpoint_id_string();
        Self { identity, key, label, key_source }
    }

    /// The node key in the form the session bus checks writers against.
    pub fn bus_key(&self) -> NodeKey {
        NodeKey(*self.identity.public_key().as_bytes())
    }
}

/// Where the app keeps its data.
#[derive(Clone, Debug)]
pub struct Dirs {
    /// `$XDG_DATA_HOME/loa`, else `~/.local/share/loa`.
    pub data: PathBuf,
    /// One journal directory per session.
    pub sessions: PathBuf,
    /// omp's per session state (memory overlay, context files).
    pub omp: PathBuf,
}

impl Dirs {
    pub fn new(data: PathBuf) -> Self {
        Self { sessions: data.join("sessions"), omp: data.join("omp"), data }
    }
}

/// The services, built once and handed to every area through `Ctx`.
pub struct Services {
    pub node: Node,
    pub dirs: Dirs,
    /// Session journals under `dirs.sessions`, opened as writers on one bus.
    /// Also the `SessionStore` fork, rewind, compaction and subagents use.
    pub library: Arc<Library>,
    /// Decides peer requests on sessions this node owns.
    pub peer_access: Arc<PeerAccess>,
    pub harnesses: Harnesses,
    /// Directory new sessions run in; the active project's folder.
    pub cwd: PathBuf,
    /// Name of the active project. At launch the last component of `cwd`.
    pub project: String,
    /// The folder the app was launched in, the workspace project's folder.
    pub workspace: PathBuf,
    /// The central config file, `<data>/config.toml`. Each area reads it
    /// and saves only the table it owns.
    pub config: ConfigStore,
    /// The MCP servers the app runs (registered in the `mcp_servers`
    /// table, plus the app-hosted ones). The Tools area registers and
    /// controls them; other areas read their tools and resources.
    pub mcp: loa_mcp::Manager,
    /// The grants this node issued and received, `<data>/grants.db`. The
    /// peers area issues and revokes into it; the Tools area reads what each
    /// peer may use.
    pub grants: Arc<loa_grants::GrantStore>,
}

impl Services {
    /// Builds the services of this machine: the node key from the OS
    /// keychain, the data directory and the installed harnesses. Returns
    /// the services and warnings to show the user.
    pub fn load() -> Result<(Self, Vec<String>), String> {
        let mut warnings = Vec::new();
        let dirs = Dirs::new(host::data_dir());
        // The OS keychain, else `<data>/node.key`; a key in memory only when neither works.
        let node = match load_app_identity(&dirs.data) {
            Ok((identity, source)) => Node::stored(identity, hostname(), source),
            Err(error) => {
                warnings.push(format!("Neither the OS keychain nor the key file works ({error}); this node uses a temporary key until restart"));
                let identity = NodeIdentity::generate().map_err(|error| format!("Can't create a node key: {error}"))?;
                Node::stored(identity, hostname(), KeySource::Memory { reason: error.to_string() })
            }
        };
        let harnesses = Harnesses::detect(&node.key, &dirs.omp);
        let cwd = starting_folder(std::env::args().nth(1), std::env::temp_dir());
        Ok((Self::new(node, dirs, harnesses, cwd)?, warnings))
    }

    /// Services over explicit parts; creates the session store. Peer
    /// requests on this node's sessions are refused until grants exist.
    pub fn new(node: Node, dirs: Dirs, harnesses: Harnesses, cwd: PathBuf) -> Result<Self, String> {
        let store = Store::new(&dirs.sessions, JournalOptions::default())
            .map_err(|error| format!("Can't open the session store {}: {error}", dirs.sessions.display()))?;
        let peer_access = Arc::new(PeerAccess::default());
        let library = Library::new(store, Arc::new(Bus::new()), node.bus_key(), Arc::clone(&peer_access) as Arc<dyn Authorize>);
        let project = cwd.file_name().map_or_else(|| cwd.display().to_string(), |name| name.to_string_lossy().into_owned());
        let config = ConfigStore::in_dir(&dirs.data);
        let mcp = loa_mcp::Manager::new(loa_mcp::Options::default())
            .map_err(|error| format!("Can't set up the MCP client: {error}"))?;
        let grants_path = dirs.data.join("grants.db");
        let grants = loa_grants::GrantStore::open(&grants_path)
            .map_err(|error| format!("Can't open the grant store {}: {error}", grants_path.display()))?;
        Ok(Self {
            node,
            dirs,
            library: Arc::new(library),
            peer_access,
            harnesses,
            workspace: cwd.clone(),
            cwd,
            project,
            config,
            mcp,
            grants: Arc::new(grants),
        })
    }

    /// Name of the workspace project, the last component of `workspace`.
    pub fn workspace_project(&self) -> String {
        self.workspace
            .file_name()
            .map_or_else(|| self.workspace.display().to_string(), |name| name.to_string_lossy().into_owned())
    }

    /// Makes `project` and its folder `cwd` the active ones: new sessions
    /// start there. Running sessions keep their own folder.
    pub fn set_active(&mut self, project: String, cwd: PathBuf) {
        self.project = project;
        self.cwd = cwd;
    }
}

/// The access check every session writer asks about peer requests. Refuses
/// everything until [`PeerAccess::set`] installs a grants-backed check.
pub struct PeerAccess(RwLock<Arc<dyn Authorize>>);

impl PeerAccess {
    /// Decides peer requests with `authorize` from now on, on every session.
    pub fn set(&self, authorize: Arc<dyn Authorize>) {
        *self.0.write_ok() = authorize;
    }
}

impl Default for PeerAccess {
    fn default() -> Self {
        Self(RwLock::new(Arc::new(NoGrants)))
    }
}

impl Authorize for PeerAccess {
    fn authorize(&self, session: &SessionId, requester: &NodeKey, draft: &Draft, dom: &Dom) -> Result<(), Denied> {
        let authorize = Arc::clone(&self.0.read_ok());
        authorize.authorize(session, requester, draft, dom)
    }
}

/// Refuses every peer request: no grants are issued on this node.
struct NoGrants;

impl Authorize for NoGrants {
    fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
        Err(Denied("this node has issued no grants".to_owned()))
    }
}

/// The folder the app starts in: a folder given as the first argument
/// (`loa-app ~/code/project`), else the system's temporary folder
/// (`$TMPDIR`, usually `/tmp`), so nothing lands in wherever the app
/// happened to be launched from until a folder is picked.
pub fn starting_folder(arg: Option<String>, temp: PathBuf) -> PathBuf {
    arg.map(PathBuf::from).filter(|path| path.is_dir()).and_then(|path| path.canonicalize().ok()).unwrap_or(temp)
}

#[cfg(test)]
mod starting_folder_tests {
    use super::starting_folder;

    #[test]
    fn starts_in_the_temp_folder_unless_a_folder_is_given() {
        let temp = std::env::temp_dir();
        let given = tempfile::tempdir().unwrap();
        assert_eq!(starting_folder(None, temp.clone()), temp);
        assert_eq!(starting_folder(Some("/no/such/folder".into()), temp.clone()), temp);
        assert_eq!(starting_folder(Some(given.path().display().to_string()), temp.clone()), given.path().canonicalize().unwrap());
    }
}
