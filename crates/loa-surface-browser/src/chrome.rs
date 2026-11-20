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
        let home = loa_core::home_dir();
        find_chrome(path.as_deref(), home.as_deref(), false).map(Self::new).ok_or(BrowserError::NoBrowser)
    }
}

/// Executable names looked up on the search path, best first. Windows adds
/// `.exe` itself (see [`loa_exec::find_executable_in`]).
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
#[cfg(not(windows))]
const FIXED_PATHS: [&str; 4] =
    ["/opt/google/chrome/chrome", "/usr/lib/chromium/chromium", "/usr/lib/chromium-browser/chromium-browser", "/snap/bin/chromium"];

/// Install locations under environment variables, as `(variable, relative path)`.
#[cfg(windows)]
const WINDOWS_INSTALLS: [(&str, &str); 6] = [
    ("ProgramFiles", r"Google\Chrome\Application\chrome.exe"),
    ("ProgramFiles(x86)", r"Google\Chrome\Application\chrome.exe"),
    ("LOCALAPPDATA", r"Google\Chrome\Application\chrome.exe"),
    ("ProgramFiles(x86)", r"Microsoft\Edge\Application\msedge.exe"),
    ("ProgramFiles", r"Microsoft\Edge\Application\msedge.exe"),
    ("LOCALAPPDATA", r"Microsoft\Edge\Application\msedge.exe"),
];

/// Playwright's full Chromium inside `chromium-<rev>`, per platform.
#[cfg(not(windows))]
const PLAYWRIGHT_CHROMIUM: [&str; 1] = ["chrome-linux64/chrome"];
#[cfg(windows)]
const PLAYWRIGHT_CHROMIUM: [&str; 2] = ["chrome-win64/chrome.exe", "chrome-win/chrome.exe"];

/// Playwright's headless shell inside `chromium_headless_shell-<rev>`.
#[cfg(not(windows))]
const PLAYWRIGHT_SHELL: [&str; 1] = ["chrome-headless-shell-linux64/chrome-headless-shell"];
#[cfg(windows)]
const PLAYWRIGHT_SHELL: [&str; 2] =
    ["chrome-headless-shell-win64/chrome-headless-shell.exe", "chrome-headless-shell-win/chrome-headless-shell.exe"];

/// Looks up an environment variable.
type Env<'a> = &'a dyn Fn(&str) -> Option<std::ffi::OsString>;

/// Every Chrome-like executable that exists, best first: the search path in
/// `path_var` (a `$PATH` value), fixed install locations, then Playwright's
/// cache under `home`, newest revision first. With `headed`, executables that
/// can only run headless (chrome-headless-shell) are left out. On Windows the
/// install locations come from `ProgramFiles`, `ProgramFiles(x86)` and
/// `LOCALAPPDATA`, and Playwright's cache is also `%LOCALAPPDATA%\ms-playwright`.
pub fn chrome_candidates(path_var: Option<&OsStr>, home: Option<&Path>, headed: bool) -> Vec<PathBuf> {
    chrome_candidates_with(path_var, home, headed, &|name| std::env::var_os(name))
}

fn chrome_candidates_with(path_var: Option<&OsStr>, home: Option<&Path>, headed: bool, env: Env) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let dirs: Vec<PathBuf> = path_var.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    for name in PATH_NAMES {
        if headed && name == "chrome-headless-shell" {
            continue;
        }
        // A bare-name search per directory: a plain file called `chromium`
        // is no browser on Windows, where only `PATHEXT` names run.
        found.extend(dirs.iter().filter_map(|dir| loa_exec::find_executable_in(Path::new(name), Some(dir.as_os_str()), None)));
    }
    found.extend(fixed_paths(env).into_iter().filter(|p| loa_exec::is_executable(p)));
    if let Some(home) = home {
        found.extend(playwright_browsers(&home.join(".cache/ms-playwright"), headed));
    }
    #[cfg(windows)]
    if let Some(local) = env("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        found.extend(playwright_browsers(&PathBuf::from(local).join("ms-playwright"), headed));
    }
    let mut seen = std::collections::HashSet::new();
    found.retain(|path| seen.insert(path.clone()));
    found
}

/// Fixed install locations to probe, best first.
#[cfg(not(windows))]
fn fixed_paths(_env: Env) -> Vec<PathBuf> {
    FIXED_PATHS.iter().map(PathBuf::from).collect()
}

#[cfg(windows)]
fn fixed_paths(env: Env) -> Vec<PathBuf> {
    WINDOWS_INSTALLS
        .iter()
        .filter_map(|(variable, relative)| env(variable).filter(|v| !v.is_empty()).map(|root| PathBuf::from(root).join(relative)))
        .collect()
}

/// The best browser [`chrome_candidates`] finds.
pub fn find_chrome(path_var: Option<&OsStr>, home: Option<&Path>, headed: bool) -> Option<PathBuf> {
    chrome_candidates(path_var, home, headed).into_iter().next()
}

/// Why the virtual display capture is not offered off Linux.
pub const VIRTUAL_DISPLAY_UNAVAILABLE: &str = "needs Linux (Xvfb)";

/// Xvfb on the search path in `path_var`. Always `None` off Linux.
pub fn find_xvfb(path_var: Option<&OsStr>) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let path_var = path_var?;
    std::env::split_paths(path_var).find_map(|dir| loa_exec::find_executable(&dir.join("Xvfb")))
}

/// Playwright installs `chromium-<rev>/<platform dir>/chrome` and
/// `chromium_headless_shell-<rev>/<platform dir>/chrome-headless-shell`
/// (`chrome-linux64`, or `chrome-win64`/`chrome-win` with `.exe` on Windows).
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
        let (rank, relatives): (u8, &[&str]) = match kind {
            "chromium" => (0, &PLAYWRIGHT_CHROMIUM),
            "chromium_headless_shell" if !headed => (1, &PLAYWRIGHT_SHELL),
            _ => continue,
        };
        if let Some(exe) = relatives.iter().map(|relative| entry.path().join(relative)).find(|exe| loa_exec::is_executable(exe)) {
            found.push((rank, revision, exe));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    found.into_iter().map(|(_, _, exe)| exe).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where Playwright puts Chromium and the headless shell below their
    /// `<name>-<rev>` folders, and the suffix of executables.
    #[cfg(not(windows))]
    const CHROMIUM: &str = "chrome-linux64/chrome";
    #[cfg(windows)]
    const CHROMIUM: &str = "chrome-win64/chrome.exe";
    #[cfg(not(windows))]
    const SHELL: &str = "chrome-headless-shell-linux64/chrome-headless-shell";
    #[cfg(windows)]
    const SHELL: &str = "chrome-headless-shell-win64/chrome-headless-shell.exe";
    const EXE: &str = if cfg!(windows) { ".exe" } else { "" };

    /// Makes an executable at `path`. Windows runs files by extension, so
    /// the file is an empty `.exe` there: discovery only looks, never runs.
    fn touch_exe(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if cfg!(windows) {
            std::fs::write(path, "").unwrap();
        } else {
            loa_exec::write_executable(path, "#!/bin/sh\n").unwrap();
        }
    }

    #[test]
    fn path_browsers_win_over_playwright_and_newest_revision_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        let home = tmp.path().join("home");
        let cache = home.join(".cache/ms-playwright");
        let chromium = |revision: &str| cache.join(format!("chromium-{revision}")).join(CHROMIUM);
        let shell = cache.join("chromium_headless_shell-1300").join(SHELL);
        touch_exe(&chromium("900"));
        touch_exe(&chromium("1200"));
        touch_exe(&shell);

        let found = chrome_candidates_with(Some(bin.as_os_str()), Some(&home), false, &|_| None);
        let ours: Vec<_> = found.iter().filter(|p| p.starts_with(tmp.path())).cloned().collect();
        assert_eq!(ours, vec![chromium("1200"), chromium("900"), shell]);

        let on_path = bin.join(format!("chromium{EXE}"));
        touch_exe(&on_path);
        let first = chrome_candidates_with(Some(bin.as_os_str()), Some(&home), false, &|_| None).remove(0);
        assert_eq!(first, on_path);
    }

    #[test]
    fn headed_discovery_skips_the_headless_shell_and_non_executables() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        let shell_on_path = bin.join(format!("chrome-headless-shell{EXE}"));
        touch_exe(&shell_on_path);
        // Unix: no execute bit. Windows: no executable extension.
        std::fs::write(bin.join("chromium"), "not executable").unwrap();
        let home = tmp.path().join("home");
        touch_exe(&home.join(".cache/ms-playwright/chromium_headless_shell-1").join(SHELL));

        let headed = chrome_candidates_with(Some(bin.as_os_str()), Some(&home), true, &|_| None);
        assert!(headed.iter().all(|p| !p.starts_with(tmp.path())), "{headed:?}");
        let headless = chrome_candidates_with(Some(bin.as_os_str()), Some(&home), false, &|_| None);
        assert_eq!(headless.first(), Some(&shell_on_path));
    }

    #[cfg(target_os = "linux")]
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

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn xvfb_is_never_found_off_linux() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Xvfb"), "").unwrap();
        std::fs::write(tmp.path().join("Xvfb.exe"), "").unwrap();
        assert_eq!(find_xvfb(Some(tmp.path().as_os_str())), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_install_locations_and_playwright_cache_are_searched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let (pf, pf86, local) = (root.join("pf"), root.join("pf86"), root.join("local"));
        let chrome_pf = pf.join(r"Google\Chrome\Application\chrome.exe");
        let chrome_pf86 = pf86.join(r"Google\Chrome\Application\chrome.exe");
        let chrome_local = local.join(r"Google\Chrome\Application\chrome.exe");
        let edge = pf86.join(r"Microsoft\Edge\Application\msedge.exe");
        let pw64 = local.join(r"ms-playwright\chromium-1200\chrome-win64\chrome.exe");
        let pw_old = local.join(r"ms-playwright\chromium-900\chrome-win\chrome.exe");
        let pw_shell = local.join(r"ms-playwright\chromium_headless_shell-1300\chrome-headless-shell-win64\chrome-headless-shell.exe");
        for path in [&chrome_pf, &chrome_pf86, &chrome_local, &edge, &pw64, &pw_old, &pw_shell] {
            touch_exe(path);
        }
        let env = |name: &str| match name {
            "ProgramFiles" => Some(pf.clone().into_os_string()),
            "ProgramFiles(x86)" => Some(pf86.clone().into_os_string()),
            "LOCALAPPDATA" => Some(local.clone().into_os_string()),
            _ => None,
        };
        let empty = root.join("empty");

        let found = chrome_candidates_with(Some(empty.as_os_str()), None, false, &env);
        assert_eq!(found, vec![chrome_pf, chrome_pf86, chrome_local, edge, pw64, pw_old, pw_shell.clone()]);

        let headed = chrome_candidates_with(Some(empty.as_os_str()), None, true, &env);
        assert!(!headed.contains(&pw_shell));

        // A browser on the search path beats every install location.
        let bin = root.join("bin");
        touch_exe(&bin.join("msedge.exe"));
        let first = chrome_candidates_with(Some(bin.as_os_str()), None, false, &env).remove(0);
        assert_eq!(first, bin.join("msedge.exe"));
    }
}
