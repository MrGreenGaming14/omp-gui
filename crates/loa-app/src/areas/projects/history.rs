//! Discovery of OMP terminal conversations, on-demand Loa imports, and deleting them from omp's storage.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use iced::Task;
use loa_core::{Draft, NodeId, attr};
use loa_harness::omp::{HARNESS, native::{self, ProjectSession, X_SESSION_FILE, delete_project_session, project_history, project_session}};
use loa_session::Error;
use loa_view::settings::projects::{OmpHistory, OmpHistoryRow};

use super::{Message, State};
use crate::area::{Area, Ctx, Request};

#[derive(Default)]
pub(super) struct History {
    scan: u64,
    folder: Option<PathBuf>,
    rows: Vec<ProjectSession>,
    /// The project whose history row is being opened or deleted; one row at a time.
    busy: Option<String>,
    changing: HashSet<String>,
}

impl History {
    pub(super) fn matches_folder(&self, folder: Option<&std::path::Path>) -> bool { self.folder.as_deref() == folder }

    pub(super) fn begin_change(&mut self, project: &str) -> bool {
        self.busy.as_deref() != Some(project) && self.changing.insert(project.to_owned())
    }

    pub(super) fn end_change(&mut self, project: &str) { self.changing.remove(project); }
}

impl State {
    pub(super) fn clear_history(&mut self) {
        self.history.scan += 1;
        self.history.folder = None;
        self.history.rows.clear();
        if let Some(screen) = &mut self.screen { screen.set_omp_history(None); }
    }

    pub(super) fn scan_history(&mut self, folder: Option<PathBuf>, ctx: &Ctx) -> Task<Message> {
        let history = &mut self.history;
        history.scan += 1;
        let scan = history.scan;
        history.folder = folder.clone();
        history.rows.clear();
        if let Some(screen) = &mut self.screen {
            screen.set_omp_history(folder.as_ref().map(|_| OmpHistory::Loading));
            screen.set_omp_history_busy(history.busy.is_some());
        }
        let Some(folder) = folder else { return Task::none() };
        let backend = ctx.backends.models.clone();
        let sessions = self.sessions.clone();
        Task::perform(async move {
            let files = backend.files().await?;
            let mut rows = tokio::task::spawn_blocking(move || project_history(&files.agent_dir, &folder))
                .await.map_err(|error| error.to_string())?.map_err(|error| error.to_string())?;
            if !rows.is_empty() {
                let imported = sessions.native_sessions(HARNESS).await.map_err(|error| error.to_string())?;
                rows.retain(|row| !imported.contains_key(&row.id));
            }
            Ok(rows)
        }, move |result| Message::HistoryLoaded(scan, result))
    }

    pub(super) fn history_loaded(&mut self, scan: u64, result: Result<Vec<ProjectSession>, String>) {
        if scan != self.history.scan { return; }
        let Some(screen) = &mut self.screen else { return };
        let view = match result {
            Ok(rows) => {
                let view = OmpHistory::Ready(rows.iter().map(|row| OmpHistoryRow {
                    id: row.id.clone(), title: row.title.clone(), modified: row.modified,
                }).collect());
                self.history.rows = rows;
                view
            }
            Err(error) => OmpHistory::Failed(error),
        };
        screen.set_omp_history(Some(view));
    }

    /// Claims the history for one operation on the shown project's listed row `native_id`: its project, folder and row.
    fn claim_row(&mut self, native_id: &str, ctx: &mut Ctx) -> Option<(String, PathBuf, ProjectSession)> {
        if self.history.busy.is_some() || self.creating.is_some() || ctx.screen() != Some(Self::ID) { return None; }
        let screen = self.screen.as_mut()?;
        let folder = self.history.folder.clone()?;
        let row = self.history.rows.iter().find(|row| row.id == native_id)?.clone();
        let project = screen.project().to_owned();
        if self.history.changing.contains(&project) {
            ctx.notify("Wait for the current project operation to finish");
            return None;
        }
        self.history.busy = Some(project.clone());
        screen.set_omp_history_busy(true);
        Some((project, folder, row))
    }

    fn release_row(&mut self) {
        self.history.busy = None;
        if let Some(screen) = &mut self.screen { screen.set_omp_history_busy(false); }
    }

    /// Lists the shown project's history again, unless the user left its screen.
    fn refresh_history(&mut self, ctx: &Ctx) -> Task<Message> {
        if self.shown().is_some() && ctx.screen() == Some(Self::ID) { self.scan_history(self.history.folder.clone(), ctx) } else { Task::none() }
    }

    pub(super) fn open_history(&mut self, native_id: &str, ctx: &mut Ctx) -> Task<Message> {
        let Some((project, folder, row)) = self.claim_row(native_id, ctx) else { return Task::none() };
        let scan = self.history.scan;
        let sessions = self.sessions.clone();
        let library = Arc::clone(ctx.runtime.library());
        let owner = ctx.services.node.key.clone();
        Task::perform(async move {
            if let Some(id) = sessions.native_sessions(HARNESS).await.map_err(|error| error.to_string())?.remove(&row.id) {
                return Ok(id);
            }
            tokio::task::spawn_blocking(move || {
                let cwd = loa_util::fs::canonicalize(&folder).map_err(|error| error.to_string())?;
                let id = loa_runtime::session::new_session_id();
                library.import_staged(&id, &owner, &project, |staged, target, sink| {
                    let name = "terminal-history.jsonl";
                    let copy = staged.join(name);
                    let io = |error: std::io::Error| Error::Port(error.to_string());
                    copy_session(&row.path, &copy).map_err(io)?;
                    let session = project_session(&copy, &cwd).map_err(io)?;
                    if !session.is_some_and(|session| session.id == row.id) {
                        return Err(Error::Port("The terminal session changed since this project was opened".into()));
                    }
                    native::import(&copy, sink, &owner)?;
                    // The OMP importer inserts the session root first.
                    sink.commit(Draft::set(NodeId(1), X_SESSION_FILE, target.join(name).to_string_lossy().as_ref()))?;
                    sink.commit(Draft::set(NodeId(1), attr::CWD, cwd.to_string_lossy().as_ref()))?;
                    Ok(())
                }).map_err(|error| error.to_string())?;
                Ok(id)
            }).await.map_err(|error| error.to_string())?
        }, move |result| Message::HistoryOpened(scan, result))
    }

    pub(super) fn history_opened(&mut self, scan: u64, result: Result<String, String>, ctx: &mut Ctx) -> Task<Message> {
        self.release_row();
        match result {
            Ok(id) => {
                if scan == self.history.scan && self.shown().is_some() && ctx.screen() == Some(Self::ID) {
                    ctx.close_screen();
                    ctx.request(Request::OpenSession(id));
                }
                let refresh = self.refresh_history(ctx);
                Task::batch([self.list(), refresh])
            }
            Err(error) => {
                ctx.notify(format!("Can't open OMP terminal session: {error}"));
                Task::none()
            }
        }
    }

    /// Deletes a listed row's conversation from omp's storage; Loa's imported copies stay.
    pub(super) fn delete_history(&mut self, native_id: &str, ctx: &mut Ctx) -> Task<Message> {
        let Some((_, folder, row)) = self.claim_row(native_id, ctx) else { return Task::none() };
        let backend = ctx.backends.models.clone();
        Task::perform(async move {
            let files = backend.files().await?;
            tokio::task::spawn_blocking(move || delete_project_session(&files.agent_dir, &folder, &row.id))
                .await.map_err(|error| error.to_string())?.map_err(|error| error.to_string())
        }, Message::HistoryDeleted)
    }

    /// Lists the history again also after a failure: other copies may be gone, the one that failed stays listed.
    pub(super) fn history_deleted(&mut self, result: Result<(), String>, ctx: &mut Ctx) -> Task<Message> {
        self.release_row();
        if let Err(error) = result { ctx.notify(format!("Can't delete OMP terminal session: {error}")); }
        self.refresh_history(ctx)
    }
}

fn copy_session(source: &std::path::Path, target: &std::path::Path) -> std::io::Result<()> {
    let mut source = std::fs::File::open(source)?;
    let mut options = std::fs::File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut output = options.open(target)?;
    std::io::copy(&mut source, &mut output)?;
    output.sync_all()
}
