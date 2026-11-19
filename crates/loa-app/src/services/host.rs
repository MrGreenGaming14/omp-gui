//! Facts about the machine the app runs on: its name and where app data
//! lives.

use std::path::PathBuf;

/// Name of this machine: the kernel host name, else `HOSTNAME` or
/// `COMPUTERNAME`, else "this device".
pub fn hostname() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .chain(["HOSTNAME", "COMPUTERNAME"].into_iter().filter_map(|key| std::env::var(key).ok()))
        .map(|name| name.trim().to_owned())
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "this device".to_owned())
}

/// The app's data directory: `$XDG_DATA_HOME/loa`, else `~/.local/share/loa`,
/// else `%APPDATA%\loa`, else under the temp directory. Not created here.
pub fn data_dir() -> PathBuf {
    let base = env_dir("XDG_DATA_HOME")
        .or_else(|| env_dir("HOME").map(|home| home.join(".local").join("share")))
        .or_else(|| env_dir("APPDATA"))
        .unwrap_or_else(std::env::temp_dir);
    base.join("loa")
}

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key).map(PathBuf::from).filter(|path| path.is_absolute())
}
