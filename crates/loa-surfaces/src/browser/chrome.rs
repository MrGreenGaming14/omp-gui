//! Browser configuration and discovery of Chrome and Xvfb executables.

use loa_util::exec::is_executable;
use crate::browser::{BrowserError, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Which capture strategy tabs use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureConfig {
    /// Headless Chrome, frames from `Page.startScreencast` as JPEG.
    Screencast,
    /// Headed Chrome on its own Xvfb display per tab, captured as raw RGBA.
    VirtualDisplay(VirtualDisplayConfig),
}

/// Settings of the virtual display fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualDisplayConfig {
    /// The Xvfb executable.
    pub xvfb: PathBuf,
    /// Directory holding the X server sockets (`X<n>`).
    pub socket_dir: PathBuf,
    /// Upper bound on captures per second.
    pub fps: u32,
}

impl VirtualDisplayConfig {
    /// Xvfb at `xvfb`, sockets in the standard `/tmp/.X11-unix`, 60 captures per second.
    pub fn new(xvfb: impl Into<PathBuf>) -> Self {
        Self { xvfb: xvfb.into(), socket_dir: PathBuf::from("/tmp/.X11-unix"), fps: 60 }
    }
}

/// How the browser server launches Chrome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserConfig {
    /// Chrome, Chromium or chrome-headless-shell.
    pub executable: PathBuf,
    /// Parent directory of the throwaway profile directories. `None` uses the
    /// system temp directory. Profiles are deleted on shutdown.
    pub profile_root: Option<PathBuf>,
    /// Chrome's sandbox. Turn off only where it cannot run (tests, containers).
    pub sandbox: bool,
    /// Size of new tabs in surface pixels.
    pub width: u32,
    pub height: u32,
    /// Screencast JPEG quality, 0 to 100.
    pub jpeg_quality: u8,
    pub capture: CaptureConfig,
    /// Passed to Chrome after the built in flags.
    pub extra_args: Vec<String>,
}

impl BrowserConfig {
    /// Headless screencast at 1280x720 with the sandbox on.
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
            profile_root: None,
            sandbox: true,
            width: 1280,
            height: 720,
            jpeg_quality: 80,
            capture: CaptureConfig::Screencast,
            extra_args: Vec::new(),
        }
    }

    /// Looks for a browser on `$PATH`, in the usual install locations and in
    /// the Playwright cache under `$HOME`. This is the only place the crate
    /// reads the environment.
    pub fn discover() -> Result<Self> {
        let path = std::env::var_os("PATH");
        let home = std::env::var_os("HOME").map(PathBuf::from);
        find_chrome(path.as_deref(), home.as_deref(), false).map(Self::new).ok_or(BrowserError::NoBrowser)
    }
}

/// Executable names looked up on the search path, best first.
const PATH_NAMES: [&str; 7] = [
    "google-chrome-stable",
    "google-chrome",
    "chromium",
    "chromium-browser",
    "chrome",
    "msedge",
    "chrome-headless-shell",
];

/// Install locations that are often not on the search path.
const FIXED_PATHS: [&str; 4] =
    ["/opt/google/chrome/chrome", "/usr/lib/chromium/chromium", "/usr/lib/chromium-browser/chromium-browser", "/snap/bin/chromium"];

/// Every Chrome-like executable that exists, best first: the search path in
/// `path_var` (a `$PATH` value), fixed install locations, then Playwright's
/// cache under `home`, newest revision first. With `headed`, executables that
/// can only run headless (chrome-headless-shell) are left out.
pub fn chrome_candidates(path_var: Option<&OsStr>, home: Option<&Path>, headed: bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let dirs: Vec<PathBuf> = path_var.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    for name in PATH_NAMES {
        if headed && name == "chrome-headless-shell" {
            continue;
        }
        found.extend(dirs.iter().map(|dir| dir.join(name)).filter(|p| is_executable(p)));
    }
    found.extend(FIXED_PATHS.iter().map(PathBuf::from).filter(|p| is_executable(p)));
    if let Some(home) = home {
        found.extend(playwright_browsers(&home.join(".cache/ms-playwright"), headed));
    }
    found.dedup();
    found
}

/// The best browser [`chrome_candidates`] finds.
pub fn find_chrome(path_var: Option<&OsStr>, home: Option<&Path>, headed: bool) -> Option<PathBuf> {
    chrome_candidates(path_var, home, headed).into_iter().next()
}

/// Xvfb on the search path in `path_var`.
pub fn find_xvfb(path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var).map(|dir| dir.join("Xvfb")).find(|p| is_executable(p))
}

/// Playwright installs `chromium-<rev>/chrome-linux64/chrome` and
/// `chromium_headless_shell-<rev>/chrome-headless-shell-linux64/chrome-headless-shell`.
/// Full Chromium comes before the headless shell, higher revisions first.
fn playwright_browsers(cache: &Path, headed: bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return Vec::new();
    };
    let mut found: Vec<(u8, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some((kind, revision)) = name.rsplit_once('-') else { continue };
        let Ok(revision) = revision.parse::<u64>() else { continue };
        let (rank, exe) = match kind {
            "chromium" => (0, entry.path().join("chrome-linux64/chrome")),
            "chromium_headless_shell" if !headed => {
                (1, entry.path().join("chrome-headless-shell-linux64/chrome-headless-shell"))
            }
            _ => continue,
        };
        if is_executable(&exe) {
            found.push((rank, revision, exe));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    found.into_iter().map(|(_, _, exe)| exe).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch_exe(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        loa_util::exec::write_executable(path, "#!/bin/sh\n").unwrap();
    }

    #[test]
    fn discovery_ranks_path_over_playwright_and_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        let home = tmp.path().join("home");
        let cache = home.join(".cache/ms-playwright");
        let shell = cache.join("chromium_headless_shell-1300/chrome-headless-shell-linux64/chrome-headless-shell");
        let revisions = ["chromium-1200", "chromium-900"].map(|dir| cache.join(dir).join("chrome-linux64/chrome"));
        revisions.iter().chain([&shell]).for_each(|exe| touch_exe(exe));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("chrome"), "not executable").unwrap();
        let ours = |headed| {
            let found = chrome_candidates(Some(bin.as_os_str()), Some(&home), headed);
            found.into_iter().filter(|p| p.starts_with(tmp.path())).collect::<Vec<_>>()
        };
        assert_eq!(ours(false), [&revisions[..], std::slice::from_ref(&shell)].concat());
        assert!(!ours(true).contains(&shell), "headed discovery skips the headless shell");
        touch_exe(&bin.join("chromium"));
        assert_eq!(find_chrome(Some(bin.as_os_str()), Some(&home), false), Some(bin.join("chromium")));
        assert_eq!(find_xvfb(Some(bin.as_os_str())), None);
        touch_exe(&bin.join("Xvfb"));
        assert_eq!(find_xvfb(Some(bin.as_os_str())), Some(bin.join("Xvfb")));
    }
}
