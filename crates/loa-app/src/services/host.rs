//! Facts about the machine the app runs on, and the ports to the programs it asks.

use std::path::PathBuf;
use std::sync::Arc;

use crate::app::usage::{OmpUsage, UsageSource};
use crate::app::workspace::{DialogPicker, FolderPicker, GitCli, GitProbe};

/// The external programs the app asks: git, the desktop folder dialog, `omp usage`.
#[derive(Clone)]
pub struct Host {
    pub git: Arc<dyn GitProbe>,
    pub picker: Arc<dyn FolderPicker>,
    pub usage: Arc<dyn UsageSource>,
}

impl Host {
    /// The command line adapters of this machine.
    pub fn system() -> Self {
        Self { git: Arc::new(GitCli), picker: Arc::new(DialogPicker), usage: Arc::new(OmpUsage) }
    }
}

/// Name of this machine: the kernel host name, else `HOSTNAME` or `COMPUTERNAME`, else "this device".
pub fn hostname() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .chain(["HOSTNAME", "COMPUTERNAME"].into_iter().filter_map(|key| std::env::var(key).ok()))
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "this device".to_owned())
}

/// The app's data directory, `loa` under the platform data dir, else under the temp directory.
pub fn data_dir() -> PathBuf {
    loa_util::fs::data_dir("loa").unwrap_or_else(|| std::env::temp_dir().join("loa"))
}
