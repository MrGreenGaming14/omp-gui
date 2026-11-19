//! Browser configuration and discovery of Chrome and Xvfb executables.

use crate::{BrowserError, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Where frames come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capture {
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
    pub capture: Capture,
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
            capture: Capture::Screencast,
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

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn touch_exe(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn path_browsers_win_over_playwright_and_newest_revision_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        let home = tmp.path().join("home");
        let cache = home.join(".cache/ms-playwright");
        touch_exe(&cache.join("chromium-900/chrome-linux64/chrome"));
        touch_exe(&cache.join("chromium-1200/chrome-linux64/chrome"));
        touch_exe(&cache.join("chromium_headless_shell-1300/chrome-headless-shell-linux64/chrome-headless-shell"));

        let found = chrome_candidates(Some(bin.as_os_str()), Some(&home), false);
        let ours: Vec<_> = found.iter().filter(|p| p.starts_with(tmp.path())).cloned().collect();
        assert_eq!(
            ours,
            vec![
                cache.join("chromium-1200/chrome-linux64/chrome"),
                cache.join("chromium-900/chrome-linux64/chrome"),
                cache.join("chromium_headless_shell-1300/chrome-headless-shell-linux64/chrome-headless-shell"),
            ]
        );

        touch_exe(&bin.join("chromium"));
        let first = find_chrome(Some(bin.as_os_str()), Some(&home), false).unwrap();
        assert_eq!(first, bin.join("chromium"));
    }

    #[test]
    fn headed_discovery_skips_the_headless_shell_and_non_executables() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        touch_exe(&bin.join("chrome-headless-shell"));
        std::fs::write(bin.join("chromium"), "not executable").unwrap();
        let home = tmp.path().join("home");
        touch_exe(&home.join(".cache/ms-playwright/chromium_headless_shell-1/chrome-headless-shell-linux64/chrome-headless-shell"));

        let headed = chrome_candidates(Some(bin.as_os_str()), Some(&home), true);
        assert!(headed.iter().all(|p| !p.starts_with(tmp.path())), "{headed:?}");
        let headless = chrome_candidates(Some(bin.as_os_str()), Some(&home), false);
        assert_eq!(headless.first(), Some(&bin.join("chrome-headless-shell")));
    }

    #[test]
    fn xvfb_is_found_on_the_search_path_only() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        touch_exe(&b.join("Xvfb"));
        let path = std::env::join_paths([&a, &b]).unwrap();
        assert_eq!(find_xvfb(Some(&path)), Some(b.join("Xvfb")));
        assert_eq!(find_xvfb(Some(a.as_os_str())), None);
    }
}
