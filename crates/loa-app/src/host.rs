//! Facts about the machine the app runs on: its name, where app data lives and
//! which executables are installed.

use std::path::{Path, PathBuf};

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

/// The first executable called `name` on `PATH`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).flat_map(|dir| candidates(&dir, name)).find(|file| is_executable(file))
}

fn candidates(dir: &Path, name: &str) -> Vec<PathBuf> {
    if cfg!(windows) {
        ["exe", "cmd", "bat"].into_iter().map(|ext| dir.join(name).with_extension(ext)).collect()
    } else {
        vec![dir.join(name)]
    }
}

#[cfg(unix)]
fn is_executable(file: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    file.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(file: &Path) -> bool {
    file.is_file()
}
