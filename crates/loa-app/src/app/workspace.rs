//! The folder and project the app works in, recent folders and project tabs, and the git and
//! folder dialog ports with their command line adapters.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use iced::Task;
use loa_core::attr;
use loa_ui_panel::{diff, files};
use loa_ui_terminal as terminal;

use super::{App, Message};
use crate::areas;

/// Reads the repository a folder is in.
#[async_trait]
pub trait GitProbe: Send + Sync {
    /// The branch checked out, or a short commit id when HEAD is detached.
    fn branch(&self, dir: &Path) -> Option<String>;
    /// `git diff HEAD`; `None` outside a git work tree with a commit.
    async fn diff(&self, dir: &Path) -> Option<String>;
}

/// The desktop's folder dialog.
#[async_trait]
pub trait FolderPicker: Send + Sync {
    /// The folder chosen, starting in `start`; `Ok(None)` when cancelled.
    async fn pick(&self, start: &Path) -> Result<Option<PathBuf>, String>;
}

/// [`GitProbe`] over `.git/HEAD` and the `git` command.
pub struct GitCli;

#[async_trait]
impl GitProbe for GitCli {
    fn branch(&self, dir: &Path) -> Option<String> {
        let git = dir.ancestors().map(|dir| dir.join(".git")).find(|git| git.exists())?;
        let git_dir = if git.is_file() {
            // A worktree's `.git` file points at its git dir.
            let target = PathBuf::from(std::fs::read_to_string(&git).ok()?.strip_prefix("gitdir:")?.trim());
            if target.is_absolute() { target } else { git.parent()?.join(target) }
        } else {
            git
        };
        let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
        match head.trim().strip_prefix("ref: refs/heads/") {
            Some(branch) => Some(branch.to_owned()),
            None => head.trim().get(..7).map(str::to_owned),
        }
    }

    async fn diff(&self, dir: &Path) -> Option<String> {
        let output = tokio::process::Command::new("git").args(["diff", "--no-color", "--no-ext-diff", "HEAD"]).current_dir(dir).output().await.ok()?;
        output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// [`FolderPicker`] over zenity (GNOME), else kdialog (KDE).
pub struct DialogPicker;

#[async_trait]
impl FolderPicker for DialogPicker {
    async fn pick(&self, start: &Path) -> Result<Option<PathBuf>, String> {
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
}

/// Folders worked in this run and the project tabs.
#[derive(Default)]
pub(crate) struct Workspace {
    /// Most recent first, for the folder menu.
    recent: Vec<PathBuf>,
    /// The projects with a tab in the project strip, in order: selected this run or of a session opened.
    pub(crate) open_projects: Vec<String>,
    /// Sessions whose project was already offered a tab, so a closed tab stays closed.
    tabbed: HashSet<String>,
}

/// The composer's folder chip for `cwd`: home shortened to `~`, and the git branch.
pub(crate) fn chip(git: &dyn GitProbe, cwd: &Path) -> loa_view::session::composer::Workspace {
    let full = cwd.display().to_string();
    let path = match loa_util::fs::home().and_then(|home| cwd.strip_prefix(home).ok().map(Path::to_owned)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => full.clone(),
    };
    loa_view::session::composer::Workspace { path, full, branch: git.branch(cwd) }
}

impl App {
    /// Works in `path` from now on; an open session is replaced by a fresh one there.
    pub(crate) fn switch_folder(&mut self, path: PathBuf) -> Task<Message> {
        let path = path.canonicalize().unwrap_or(path);
        if !path.is_dir() {
            self.ctx.notify(format!("{} isn't a folder", path.display()));
            return Task::none();
        }
        if path == self.ctx.runtime.cwd() {
            return Task::none();
        }
        let moved = self.set_folder(path);
        let start = if self.ctx.runtime.sessions().active().is_some() {
            self.lifecycle(|s, ctx, areas| s.run(ctx, areas, loa_runtime::lifecycle::Command::Start { prompt: None }))
        } else {
            Task::none()
        };
        Task::batch([moved, start])
    }

    /// Points Files, Changes, the terminal, the composer and new sessions at `path`. A folder is not a project:
    /// the active project stays.
    pub(super) fn set_folder(&mut self, path: PathBuf) -> Task<Message> {
        self.ctx.runtime.set_cwd(path.clone());
        let _ = self.diff.set_note(Some("Reading changes…".to_owned()));
        self.files = files::State::new(path.clone(), Box::new(files::LocalFs));
        self.remember(&path);
        self.ctx.notify(format!("Working in {}", chip(self.ctx.services.host.git.as_ref(), &path).path));
        Task::batch([self.working_diff(), self.follow(path)])
    }

    /// Puts `path` first among the recent folders the composer lists.
    pub(super) fn remember(&mut self, path: &Path) {
        let recent = &mut self.workspace.recent;
        recent.retain(|folder| folder != path);
        recent.insert(0, path.to_owned());
        recent.truncate(8);
        let git = self.ctx.services.host.git.as_ref();
        self.ctx.composer.set_folders(recent.iter().map(|folder| chip(git, folder)).collect());
    }

    /// The composer's folder chip and the terminal follow `cwd`.
    pub(super) fn follow(&mut self, cwd: PathBuf) -> Task<Message> {
        self.ctx.composer.set_workspace(Some(chip(self.ctx.services.host.git.as_ref(), &cwd)));
        self.terminal.set_new_tab(terminal::local::LocalShell::login_shell(cwd.clone()));
        if self.terminal.is_empty() {
            return Task::none();
        }
        let here = self.terminal.tabs().find(|id| matches!(self.terminal.target(*id), Some(terminal::state::Target::Local(shell)) if shell.cwd == cwd));
        let message = match here {
            Some(id) => terminal::state::Message::Select(id),
            None => terminal::state::Message::NewTab,
        };
        self.terminal.update(message).map(Message::Terminal)
    }

    /// Makes `project` the active one, with its folder `cwd` when it has one: new sessions, new terminals, Files and
    /// Diff follow, every area is told and the choice is saved. Running sessions and open terminals keep their folder.
    pub(super) fn activate_project(&mut self, project: String, cwd: Option<PathBuf>) -> Task<Message> {
        self.ctx.shell.set_active_project(Some(project.clone()));
        self.add_project_tab(&project);
        let cwd = cwd.unwrap_or_else(|| self.ctx.runtime.cwd().to_path_buf());
        if self.ctx.runtime.project() == Some(project.as_str()) && self.ctx.runtime.cwd() == cwd {
            return Task::none();
        }
        self.ctx.runtime.set_active(project.clone(), Some(cwd.clone()));
        self.files.set_root(cwd.clone());
        self.remember(&cwd);
        let saved = loa_settings::app::ActiveProject { name: project.clone(), cwd: cwd.clone() };
        self.ctx.edit_config("settings", |config| config.active_project = Some(saved));
        let tasks: Vec<_> = self.areas.iter_mut().map(|area| area.project_activated(&project, &cwd, &mut self.ctx)).collect();
        Task::batch(tasks.into_iter().chain([self.follow(cwd), self.working_diff(), self.settle()]))
    }

    /// The composer's project chip: the shown session moves to `project`; with none shown, new sessions join it.
    pub(super) fn project_picked(&mut self, project: Option<String>) -> Task<Message> {
        if let Some(session) = self.ctx.runtime.sessions().active() {
            let session = session.id().to_string();
            return self.update(Message::area(areas::projects::Message::Move { session, project }));
        }
        match project {
            Some(project) => self.update(Message::area(areas::projects::Message::Activate(project))),
            None => {
                self.clear_project();
                Task::none()
            }
        }
    }

    /// No project is active: new sessions join none.
    fn clear_project(&mut self) {
        self.ctx.runtime.clear_project();
        self.ctx.shell.set_active_project(None);
        self.ctx.edit_config("settings", |config| config.active_project = None);
    }

    /// Drops every trace of the deleted `project`: its tab, its pin, and its place as the active project.
    pub(super) fn forget_project(&mut self, project: &str) -> Task<Message> {
        self.close_project_tab(project);
        let mut pins = self.ctx.shell.sidebar().pinned_projects().clone();
        if pins.remove(project) {
            self.ctx.shell.sidebar_mut().set_pinned_projects(pins.clone());
            self.ctx.edit_config("settings", |config| config.pinned_projects = pins);
        }
        if self.ctx.runtime.project() == Some(project) {
            self.clear_project();
        }
        Task::none()
    }

    /// Reads and parses the working tree diff of the current folder for the Diff tab, off the UI thread.
    pub(super) fn working_diff(&self) -> Task<Message> {
        let (git, cwd) = (self.ctx.services.host.git.clone(), self.ctx.runtime.cwd().to_path_buf());
        Task::perform(
            async move {
                let patch = git.diff(&cwd).await?;
                let parsed = tokio::task::spawn_blocking(move || diff::parse_unified(&patch).map_err(|error| error.to_string())).await;
                Some(parsed.unwrap_or_else(|error| Err(error.to_string())))
            },
            Message::WorkingDiff,
        )
    }

    pub(super) fn show_diff(&mut self, parsed: Option<Result<Vec<diff::FileDiff>, String>>) -> Task<Message> {
        match parsed {
            // Not a git repository (or no commit yet): say so instead of keeping another folder's changes.
            None => {
                let folder = chip(self.ctx.services.host.git.as_ref(), self.ctx.runtime.cwd()).path;
                self.diff.set_note(Some(format!("No git changes to show: {folder} isn't a git repository with a commit"))).map(Message::Diff)
            }
            Some(Ok(files)) => {
                let _ = self.diff.set_note(None);
                self.diff.set_files(files).map(Message::Diff)
            }
            Some(Err(error)) => {
                self.ctx.notify(format!("Can't read the working tree diff: {error}"));
                Task::none()
            }
        }
    }

    /// Asks for a folder with the desktop's folder dialog.
    pub(super) fn browse(&self) -> Task<Message> {
        let (picker, cwd) = (self.ctx.services.host.picker.clone(), self.ctx.runtime.cwd().to_path_buf());
        Task::perform(async move { picker.pick(&cwd).await }, Message::FolderChosen)
    }

    /// Gives `project` a tab in the project strip when it has none.
    fn add_project_tab(&mut self, project: &str) {
        if !self.workspace.open_projects.iter().any(|open| open == project) {
            self.workspace.open_projects.push(project.to_owned());
            self.ctx.shell.set_open_projects(self.workspace.open_projects.clone());
        }
    }

    /// Drops the tab of `project`; it gets one again only when activated again.
    pub(super) fn close_project_tab(&mut self, project: &str) {
        self.workspace.open_projects.retain(|open| open != project);
        self.ctx.shell.set_open_projects(self.workspace.open_projects.clone());
    }

    /// Gives a tab to the project of every session opened since the last call.
    pub(super) fn sync_project_tabs(&mut self) {
        let mut seen = Vec::new();
        for session in self.ctx.runtime.sessions().iter().filter(|session| !self.workspace.tabbed.contains(session.id().as_str())) {
            // A session whose journal has no root yet is looked at again next time.
            if let Some(project) = session.writer().with_dom(|dom| dom.root().map(|root| root.str_attr(attr::PROJECT).map(str::to_owned))) {
                seen.push((session.id().to_string(), project));
            }
        }
        for (id, project) in seen {
            self.workspace.tabbed.insert(id);
            if let Some(project) = project.filter(|project| !project.is_empty()) {
                self.add_project_tab(&project);
            }
        }
    }

    /// Activates the project tab after (or before) the active project's, wrapping around.
    pub(super) fn step_project_tab(&mut self, forward: bool) -> Task<Message> {
        if !self.ctx.shell.project_tabs() {
            self.ctx.notify("Project tabs are off; turn them on from the palette");
            return Task::none();
        }
        let tabs = &self.workspace.open_projects;
        let Some(last) = tabs.len().checked_sub(1) else {
            self.ctx.notify("No project tabs are open");
            return Task::none();
        };
        let index = match (tabs.iter().position(|open| self.ctx.runtime.project() == Some(open.as_str())), forward) {
            (Some(index), true) => if index == last { 0 } else { index + 1 },
            (Some(index), false) => index.checked_sub(1).unwrap_or(last),
            (None, true) => 0,
            (None, false) => last,
        };
        let project = tabs[index].clone();
        self.update(Message::area(areas::projects::Message::Open(project)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_branch_and_shortens_home() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".git")).unwrap();
        std::fs::write(repo.path().join(".git/HEAD"), "ref: refs/heads/tai/ui-v3\n").unwrap();
        std::fs::create_dir_all(repo.path().join("crates/app")).unwrap();
        assert_eq!(GitCli.branch(&repo.path().join("crates/app")).as_deref(), Some("tai/ui-v3"), "found from a subfolder");
        std::fs::write(repo.path().join(".git/HEAD"), "0123456789abcdef\n").unwrap();
        assert_eq!(GitCli.branch(repo.path()).as_deref(), Some("0123456"), "detached HEAD shows a short id");
        assert_eq!(GitCli.branch(tempfile::tempdir().unwrap().path()), None);
        if let Some(home) = loa_util::fs::home() {
            assert_eq!(chip(&GitCli, &home.join("Projects/omp-gui")).path, "~/Projects/omp-gui");
        }
    }
}
