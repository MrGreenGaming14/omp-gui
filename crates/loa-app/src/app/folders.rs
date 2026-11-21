//! The working folder: switching it, the recent folders, the terminal that
//! follows it, and its git branch and working tree diff.

use super::*;

impl App {
    /// Works in `path` from now on: Files, Changes and new sessions follow,
    /// and an open session is replaced by a fresh one there, since a
    /// running harness can't move.
    pub(crate) fn switch_folder(&mut self, path: PathBuf) -> Task<Message> {
        let path = path.canonicalize().unwrap_or(path);
        if !path.is_dir() {
            self.ctx.notify(format!("{} isn't a folder", path.display()));
            return Task::none();
        }
        if path == self.ctx.services.cwd {
            return Task::none();
        }
        let moved = self.set_folder(path.clone());
        let start = if self.ctx.sessions.active().is_some() { self.start(None) } else { Task::none() };
        Task::batch([moved, start])
    }

    /// Points Files, Changes, the terminal, the composer and new sessions
    /// at `path`, without touching the open session.
    pub(super) fn set_folder(&mut self, path: PathBuf) -> Task<Message> {
        self.ctx.services.cwd = path.clone();
        let _ = self.diff.set_note(Some("Reading changes…".to_owned()));
        self.files = files::State::new(path.clone(), Box::new(files::LocalFs));
        self.recent_folders.retain(|folder| *folder != path);
        self.recent_folders.insert(0, path.clone());
        self.recent_folders.truncate(8);
        let folder = workspace(&path);
        self.ctx.notify(format!("Working in {}", folder.path));
        self.ctx.composer.set_workspace(Some(folder));
        self.refresh_folders();
        let terminal = self.terminal_follow(path.clone());
        Task::batch([working_diff(path), terminal])
    }

    pub(super) fn refresh_folders(&mut self) {
        let folders = self.recent_folders.iter().map(|folder| workspace(folder)).collect();
        self.ctx.composer.set_folders(folders);
    }

    /// Points the Terminal tab at `cwd`: + opens shells there, and when a
    /// shell is open somewhere else, one opens (or is picked) in `cwd`.
    pub(super) fn terminal_follow(&mut self, cwd: PathBuf) -> Task<Message> {
        self.terminal.set_new_tab(terminal::local::LocalShell::login_shell(cwd.clone()));
        if self.terminal.is_empty() {
            return Task::none();
        }
        let here = self.terminal.tabs().find(|id| matches!(self.terminal.target(*id), Some(terminal::state::Target::Local(shell)) if shell.cwd == cwd));
        let message = match here {
            Some(id) => terminal::state::Message::Select(id),
            None => terminal::state::Message::NewTab,
        };
        self.terminal.update(message).0.map(Message::Terminal)
    }
}

/// Asks for a folder with the desktop's folder dialog: zenity (GNOME), else
/// kdialog (KDE). `Ok(None)` when the dialog was cancelled.
pub(crate) async fn choose_folder(start: PathBuf) -> Result<Option<PathBuf>, String> {
    let start = format!("{}/", start.display());
    let dialogs: [(&str, Vec<&str>); 2] = [
        ("zenity", vec!["--file-selection", "--directory", "--title=Choose a folder", "--filename", start.as_str()]),
        ("kdialog", vec!["--getexistingdirectory", start.as_str(), "--title", "Choose a folder"]),
    ];
    for (program, args) in dialogs {
        match tokio::process::Command::new(program).args(&args).output().await {
            Ok(output) if output.status.success() => {
                let chosen = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                return Ok((!chosen.is_empty()).then(|| PathBuf::from(chosen)));
            }
            // Cancelled.
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("The folder dialog failed: {error}")),
        }
    }
    Err("No folder dialog found: install zenity or kdialog, or pick a recent folder".to_owned())
}

/// The composer's folder chip for `cwd`: home shortened to `~`, and the git
/// branch read from the repository's HEAD.
pub(super) fn workspace(cwd: &Path) -> composer::Workspace {
    let full = cwd.display().to_string();
    let path = match loa_core::home_dir() {
        Some(home) if cwd.starts_with(&home) => match cwd.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => full.clone(),
        },
        _ => full.clone(),
    };
    composer::Workspace { path, full, branch: git_branch(cwd) }
}

/// The branch checked out in the repository holding `dir`: read from
/// `.git/HEAD`, following a worktree's `.git` file; a short commit id when
/// HEAD is detached.
fn git_branch(dir: &Path) -> Option<String> {
    let git = dir.ancestors().map(|dir| dir.join(".git")).find(|git| git.exists())?;
    let git_dir = if git.is_file() {
        let pointer = std::fs::read_to_string(&git).ok()?;
        let target = PathBuf::from(pointer.strip_prefix("gitdir:")?.trim());
        if target.is_absolute() { target } else { git.parent()?.join(target) }
    } else {
        git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => Some(branch.to_owned()),
        None => head.get(..7).map(str::to_owned),
    }
}

/// Reads `git diff HEAD` of `cwd` for the Diff tab. `None` when `cwd` isn't a
/// git work tree or git isn't installed.
pub(super) fn working_diff(cwd: PathBuf) -> Task<Message> {
    Task::perform(async move { git_diff(&cwd).await }, Message::WorkingDiff)
}

async fn git_diff(cwd: &Path) -> Option<Result<Vec<diff::FileDiff>, String>> {
    let output = tokio::process::Command::new("git")
        .args(["diff", "--no-color", "--no-ext-diff", "HEAD"])
        .current_dir(cwd)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let patch = String::from_utf8_lossy(&output.stdout).into_owned();
    // Parsing a large diff would stall the UI thread.
    let parsed = tokio::task::spawn_blocking(move || diff::parse_unified(&patch).map_err(|error| error.to_string()))
        .await
        .unwrap_or_else(|error| Err(error.to_string()));
    Some(parsed.map_err(|error| format!("Can't read the working tree diff: {error}")))
}

#[cfg(test)]
mod workspace_tests {
    use super::*;

    #[test]
    fn workspace_reads_the_branch_and_shortens_home() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        std::fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/tai/ui-v3\n").unwrap();
        std::fs::create_dir_all(repo.path().join("crates/app")).unwrap();
        assert_eq!(git_branch(&repo.path().join("crates/app")).as_deref(), Some("tai/ui-v3"), "found from a subfolder");

        std::fs::write(repo.path().join(".git/HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(git_branch(repo.path()).as_deref(), Some("0123456"), "detached HEAD shows a short id");

        let outside = tempfile::tempdir().unwrap();
        assert_eq!(git_branch(outside.path()), None);

        if let Some(home) = loa_core::home_dir() {
            assert_eq!(workspace(&home.join("Projects/omp-gui")).path, "~/Projects/omp-gui");
        }
    }
}
