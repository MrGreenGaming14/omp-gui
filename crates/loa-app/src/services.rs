//! Long-lived services created once at startup and shared by every area through `Ctx`.

use loa_util::RwLockExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use loa_bus::{Authorize, Bus, Denied, JournalOptions, NodeKey, SessionId, Store};
use loa_settings::app::ConfigStore;
use loa_core::{Dom, Draft};
use loa_session::library::Library;
use loa_trust::{KeySource, NodeIdentity, load_app_identity};

pub mod harness;
mod host;

pub use host::{Host, hostname};

/// This node: its identity, the grants it issued and received, and the MCP servers it runs.
pub struct NodeServices {
    pub identity: NodeIdentity,
    /// Public key as text, written to `<session node>`.
    pub key: String,
    /// Human name, the machine's host name.
    pub label: String,
    pub key_source: KeySource,
    /// Decides peer requests on sessions this node owns.
    pub peer_access: Arc<PeerAccess>,
    /// `<data>/grants.db`.
    pub grants: Arc<loa_trust::GrantStore>,
    pub mcp: loa_mcp::Manager,
}

impl NodeServices {
    /// The node over `identity`, its grant store in `data`.
    pub fn open(identity: NodeIdentity, label: String, key_source: KeySource, data: &Path) -> Result<Self, String> {
        // On a first run nothing has created the data directory yet.
        std::fs::create_dir_all(data).map_err(|error| format!("Can't create the data directory {}: {error}", data.display()))?;
        let grants_path = data.join("grants.db");
        let grants = loa_trust::GrantStore::open(&grants_path).map_err(|error| format!("Can't open the grant store {}: {error}", grants_path.display()))?;
        let mcp = loa_mcp::Manager::new(loa_mcp::Options::default()).map_err(|error| format!("Can't set up the MCP client: {error}"))?;
        let key = identity.public_key().to_string();
        Ok(Self { identity, key, label, key_source, peer_access: Arc::new(PeerAccess::default()), grants: Arc::new(grants), mcp })
    }

    /// The node key the session bus checks writers against.
    pub fn bus_key(&self) -> NodeKey {
        self.identity.public_key().into()
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

/// What the app keeps on disk besides the session library: the central config.
pub struct Storage {
    pub dirs: Dirs,
    /// `<data>/config.toml`; each area saves only the table it owns.
    pub config: ConfigStore,
}

impl Storage {
    pub fn new(dirs: Dirs) -> Self {
        Self { config: ConfigStore::in_dir(&dirs.data), dirs }
    }
}

/// Opens the session store in `dirs`, its writers checked by `node`.
pub fn open_library(dirs: &Dirs, node: &NodeServices) -> Result<Arc<Library>, String> {
    let store = Store::new(&dirs.sessions, JournalOptions::default())
        .map_err(|error| format!("Can't open the session store {}: {error}", dirs.sessions.display()))?;
    let library = Library::new(store, Arc::new(Bus::new()), node.bus_key(), Arc::clone(&node.peer_access) as Arc<dyn Authorize>);
    Ok(Arc::new(library))
}

pub struct Services {
    pub node: NodeServices,
    pub storage: Storage,
    /// The processes the app asks about the machine: git, the folder dialog, `omp usage`.
    pub host: Host,
    /// The folder the app was launched in, the workspace project's folder.
    pub workspace: PathBuf,
}

impl Services {
    /// The services of this machine, with warnings to show the user.
    pub fn load() -> Result<(Self, Vec<String>), String> {
        let mut warnings = Vec::new();
        let dirs = Dirs::new(host::data_dir());
        let (identity, source) = match load_app_identity(&dirs.data) {
            Ok(loaded) => loaded,
            Err(error) => {
                warnings.push(format!("Neither the OS keychain nor the key file works ({error}); this node uses a temporary key until restart"));
                let identity = NodeIdentity::generate().map_err(|error| format!("Can't create a node key: {error}"))?;
                (identity, KeySource::Memory { reason: error.to_string() })
            }
        };
        let node = NodeServices::open(identity, hostname(), source, &dirs.data)?;
        let cwd = starting_folder(std::env::args().nth(1), std::env::temp_dir());
        Ok((Self::new(node, dirs, Host::system(), cwd), warnings))
    }

    /// Services over explicit parts, launched in `workspace`; peer requests are refused until grants exist.
    pub fn new(node: NodeServices, dirs: Dirs, host: Host, workspace: PathBuf) -> Self {
        Self { node, storage: Storage::new(dirs), host, workspace }
    }

    /// Name of the workspace project.
    pub fn workspace_project(&self) -> String {
        folder_name(&self.workspace)
    }
}

/// The last component of `path`, the whole path when it has none.
pub fn folder_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

/// The access check every session writer asks about peer requests; refuses all until [`PeerAccess::set`].
pub struct PeerAccess(RwLock<Arc<dyn Authorize>>);

impl PeerAccess {
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

/// The folder the app starts in: the folder given as the first argument, else the temp folder.
pub fn starting_folder(arg: Option<String>, temp: PathBuf) -> PathBuf {
    arg.map(PathBuf::from).filter(|path| path.is_dir()).and_then(|path| path.canonicalize().ok()).unwrap_or(temp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_run_creates_the_data_directory() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("loa");
        let key_source = KeySource::Memory { reason: "test".into() };
        NodeServices::open(NodeIdentity::from_secret_bytes(&[1; 32]), "node".into(), key_source, &data).expect("the node opens");
        assert!(data.join("grants.db").exists());
    }

    #[test]
    fn starts_in_the_temp_folder_unless_a_folder_is_given() {
        let temp = std::env::temp_dir();
        let given = tempfile::tempdir().unwrap();
        assert_eq!(starting_folder(None, temp.clone()), temp);
        assert_eq!(starting_folder(Some("/no/such/folder".into()), temp.clone()), temp);
        assert_eq!(starting_folder(Some(given.path().display().to_string()), temp.clone()), given.path().canonicalize().unwrap());
    }
}
