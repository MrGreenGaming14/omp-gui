//! The projects service `loa-app` holds: listing, creating, moving and deleting projects, the project screen,
//! rebuilds, chat context and tools.

use crate::context::{ContextRequest, build_context, render_markdown};
use crate::digest::{digest_of, digest_state};
use crate::digester::{Digester, PromptLimits, Summarizers};
use crate::rank::last_active;
use crate::rebuild::{RebuildDriver, RebuildOutcome};
use crate::sessions::LiveSessions;
use crate::settings::{SettingsStore, exclude_draft, is_excluded};
use crate::source::{Access, SessionInfo, SessionSource, visible_sessions};
use crate::{Error, Result};
use loa_contracts::harness::{HostToolSpec, HostTools};
use loa_core::{Dom, Draft, ElementKind, attr};
use loa_settings::projects::{ProjectOverview, ProjectSettings, SessionRow};
use loa_util::fs::Snapshot;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// `debounce`: quiet time after a turn end before a digest rebuild; `context_budget` in tokens.
#[derive(Clone, Debug)]
pub struct ServiceConfig {
    pub debounce: Duration,
    pub context_budget: usize,
    pub limits: PromptLimits,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self { debounce: Duration::from_secs(10), context_budget: 4_000, limits: PromptLimits::default() }
    }
}

/// A project as the sidebar lists it: `cwd` is its folder when set, else that of its latest session;
/// `last_active` in Unix ms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSummary {
    pub name: String,
    pub cwd: Option<String>,
    pub sessions: usize,
    pub last_active: i64,
}

/// The project screen's row for one session.
fn session_row(info: &SessionInfo, dom: &Dom) -> SessionRow {
    let root = dom.root();
    let digest = digest_of(dom);
    SessionRow {
        id: info.id.clone(),
        title: info.title.clone(),
        harness: root.and_then(|r| r.str_attr(attr::HARNESS)).map(str::to_owned),
        created: info.created,
        last_active: last_active(dom),
        turns: root.map_or(0, |r| dom.children_of_kind(r.id, ElementKind::Turn).count()),
        excluded: is_excluded(dom),
        digest_state: digest_state(dom),
        digest_model: digest.as_ref().and_then(|d| d.model.clone()),
        digest: digest.map(|d| d.content),
    }
}

/// A project's folder as a path string, when one is set.
fn folder_of(settings: &ProjectSettings) -> Option<String> {
    settings.folder.as_ref().map(|dir| dir.to_string_lossy().into_owned())
}

/// Updates the `.omgui` links in `folder` when it exists. A failure is logged: the listing matters more.
fn sync_links(project: &str, folder: &Path, dirs: &BTreeSet<PathBuf>) {
    if folder.is_dir()
        && let Err(error) = crate::links::sync(folder, dirs)
    {
        tracing::warn!(project, folder = %folder.display(), %error, "can't update the .omgui links");
    }
}

/// Removes the `.omgui` links of `folder` when it exists, logging a failure.
fn remove_links(project: &str, folder: &Path) {
    if folder.is_dir()
        && let Err(error) = crate::links::remove(folder)
    {
        tracing::warn!(project, folder = %folder.display(), %error, "can't remove the .omgui links");
    }
}

/// `folder`, unless it is the system temp folder itself (`/tmp`): then a new directory inside it, named like
/// `/tmp/.tmpAbC123`, so the project's files and `.omgui` links don't mix with everything else there.
pub fn isolated(folder: PathBuf) -> Result<PathBuf> {
    let temp = std::env::temp_dir();
    let same = |a: &Path, b: &Path| a == b || a.canonicalize().ok().is_some_and(|a| b.canonicalize().is_ok_and(|b| a == b));
    if !same(&folder, &temp) {
        return Ok(folder);
    }
    Ok(tempfile::Builder::new().tempdir_in(&folder)?.keep())
}

/// Cheap to clone; every clone feeds the same rebuild driver.
#[derive(Clone)]
pub struct ProjectService {
    sessions: LiveSessions,
    settings: Arc<dyn SettingsStore>,
    access: Arc<dyn Access>,
    rebuild: mpsc::UnboundedSender<String>,
    context_budget: usize,
}

impl ProjectService {
    /// Starts the rebuild driver on the current runtime; it stops with the last clone.
    pub fn start(
        sessions: LiveSessions,
        settings: Arc<dyn SettingsStore>,
        access: Arc<dyn Access>,
        summarizers: Arc<dyn Summarizers>,
        config: ServiceConfig,
    ) -> (Self, mpsc::UnboundedReceiver<RebuildOutcome>) {
        let source = Arc::new(sessions.clone());
        let digester = Digester::new(summarizers, config.limits);
        let driver = RebuildDriver { source: source.clone(), writer: source, digester, debounce: config.debounce, settings: Some(settings.clone()) };
        let (rebuild, outcomes) = driver.spawn();
        (Self { sessions, settings, access, rebuild, context_budget: config.context_budget }, outcomes)
    }

    /// Queues a digest rebuild once `session` goes quiet (after a turn end or compaction).
    pub fn notify(&self, session: &str) -> bool {
        self.rebuild.send(session.to_owned()).is_ok()
    }

    /// Every project: each stored one and each a session names, most recently active first. A project's folder is
    /// its stored one when set, else the cwd of its most recent session. Also brings each stored folder's `.omgui`
    /// links up to date with its sessions' directories.
    pub async fn projects(&self) -> Result<Vec<ProjectSummary>> {
        let mut folders = BTreeMap::new();
        let mut projects: BTreeMap<String, (ProjectSummary, i64)> = BTreeMap::new();
        for name in self.settings.names().await? {
            if let Some(folder) = folder_of(&self.settings.load(&name).await?) {
                folders.insert(name.clone(), folder);
            }
            projects.insert(name.clone(), (ProjectSummary { name, cwd: None, sessions: 0, last_active: 0 }, i64::MIN));
        }
        let mut dirs: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
        for (info, dom) in self.sessions.all().await? {
            let active = last_active(&dom);
            let cwd = dom.root().and_then(|r| r.str_attr(attr::CWD)).map(str::to_owned);
            if let Some(cwd) = &cwd {
                dirs.entry(info.project.clone()).or_default().insert(PathBuf::from(cwd));
            }
            let fresh = || (ProjectSummary { name: info.project.clone(), cwd: None, sessions: 0, last_active: active }, i64::MIN);
            let (summary, cwd_at) = projects.entry(info.project.clone()).or_insert_with(fresh);
            summary.sessions += 1;
            summary.last_active = summary.last_active.max(active);
            if cwd.is_some() && active >= *cwd_at {
                summary.cwd = cwd;
                *cwd_at = active;
            }
        }
        for (name, folder) in &folders {
            sync_links(name, Path::new(folder), &dirs.remove(name).unwrap_or_default());
        }
        let mut projects: Vec<ProjectSummary> = projects
            .into_iter()
            .map(|(name, (mut summary, _))| {
                if let Some(folder) = folders.remove(&name) {
                    summary.cwd = Some(folder);
                }
                summary
            })
            .collect();
        projects.sort_by(|a, b| b.last_active.cmp(&a.last_active).then_with(|| a.name.cmp(&b.name)));
        Ok(projects)
    }

    /// Creates a project with `settings` and returns the folder it stored, which [`isolated`] may have replaced.
    /// Fails on a blank name or one a stored project already has.
    pub async fn create(&self, name: &str, settings: &ProjectSettings) -> Result<Option<PathBuf>> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::EmptyName);
        }
        if self.settings.names().await?.iter().any(|stored| stored == name) {
            return Err(Error::ProjectExists(name.to_owned()));
        }
        let mut settings = settings.clone();
        settings.folder = settings.folder.map(isolated).transpose()?;
        self.settings.save(name, &settings).await?;
        Ok(settings.folder)
    }

    /// Makes `folder` the project's folder, or a new one inside it per [`isolated`], and returns it. Its `.omgui`
    /// links move with it on the next listing; the old folder's are removed now.
    pub async fn set_folder(&self, project: &str, folder: PathBuf) -> Result<PathBuf> {
        let mut settings = self.settings.load(project).await?;
        if settings.folder.as_ref() == Some(&folder) {
            return Ok(folder);
        }
        let folder = isolated(folder)?;
        let old = settings.folder.replace(folder.clone());
        self.settings.save(project, &settings).await?;
        if let Some(old) = old {
            remove_links(project, &old);
        }
        Ok(folder)
    }

    /// Deletes `project`: its settings, its ideas notes and its folder's `.omgui` links go, and its sessions are
    /// kept, in no project. Returns how many sessions it had.
    pub async fn delete(&self, project: &str) -> Result<usize> {
        let settings = self.settings.load(project).await?;
        let mut moved = 0;
        for (info, dom) in self.sessions.all().await? {
            if info.project != project {
                continue;
            }
            if let Some(root) = dom.root() {
                self.sessions.commit(&info.id, Draft::clear(root.id, attr::PROJECT)).await?;
                moved += 1;
            }
        }
        self.settings.remove(project).await?;
        if let Some(folder) = settings.folder {
            remove_links(project, &folder);
        }
        Ok(moved)
    }

    /// Puts `session` into `project`, or out of every project when `None`; a blank name counts as `None`.
    pub async fn move_session(&self, session: &str, project: Option<&str>) -> Result<()> {
        let project = project.map(str::trim).filter(|name| !name.is_empty());
        let not_found = || Error::SessionNotFound(session.to_owned());
        let dom = SessionSource::load(&self.sessions, session).await?.ok_or_else(not_found)?;
        let root = dom.root().ok_or_else(not_found)?;
        if root.str_attr(attr::PROJECT).map(str::trim).filter(|name| !name.is_empty()) == project {
            return Ok(());
        }
        let draft = match project {
            Some(name) => Draft::set(root.id, attr::PROJECT, name),
            None => Draft::clear(root.id, attr::PROJECT),
        };
        self.sessions.commit(session, draft).await?;
        Ok(())
    }

    /// The project screen's data; `cwd` is the project's folder when set, else its latest session's. Ideas notes
    /// that can't be read are reported with the rest.
    pub async fn overview(&self, project: &str) -> Result<ProjectOverview> {
        let settings = self.settings.load(project).await?;
        let ideas = self.settings.load_ideas(project).await.map_err(|error| error.to_string());
        let (mut sessions, mut cwd) = (Vec::new(), None::<(i64, String)>);
        for info in visible_sessions(&self.sessions, &*self.access, project).await? {
            let Some(dom) = SessionSource::load(&self.sessions, &info.id).await? else { continue };
            let row = session_row(&info, &dom);
            if let Some(dir) = dom.root().and_then(|r| r.str_attr(attr::CWD))
                && cwd.as_ref().is_none_or(|(at, _)| row.last_active >= *at)
            {
                cwd = Some((row.last_active, dir.to_owned()));
            }
            sessions.push(row);
        }
        sessions.sort_by(|a, b| b.last_active.cmp(&a.last_active).then_with(|| a.id.cmp(&b.id)));
        let cwd = folder_of(&settings).or_else(|| cwd.map(|(_, dir)| dir));
        Ok(ProjectOverview { name: project.to_owned(), cwd, settings, ideas, sessions })
    }

    /// Stores the project's settings, then its ideas notes when given, with the file as seen when they were loaded;
    /// `None` leaves the stored notes as they are. Returns the notes file as written.
    pub async fn save_settings(&self, project: &str, settings: &ProjectSettings, ideas: Option<(Snapshot, &str)>) -> Result<Option<Snapshot>> {
        self.settings.save(project, settings).await?;
        match ideas {
            Some((seen, text)) => self.settings.save_ideas(project, seen, text).await.map(Some),
            None => Ok(None),
        }
    }

    /// Sets the exclude toggle (including again queues a rebuild); whether anything changed.
    pub async fn set_excluded(&self, session: &str, excluded: bool) -> Result<bool> {
        let dom = SessionSource::load(&self.sessions, session).await?.ok_or_else(|| Error::SessionNotFound(session.to_owned()))?;
        let Some(draft) = exclude_draft(&dom, excluded) else { return Ok(false) };
        self.sessions.commit(session, draft).await?;
        if !excluded {
            self.notify(session);
        }
        Ok(true)
    }

    /// Markdown context for a new chat; `query` is its first prompt or title. `None` when empty.
    pub async fn context(&self, project: &str, session: Option<&str>, query: &str) -> Result<Option<String>> {
        let request = ContextRequest { project, session, query, budget_tokens: self.context_budget, now: loa_core::now_ms() };
        let packed = build_context(&self.sessions, &*self.access, &*self.settings, &request).await?;
        let empty = packed.digests.is_empty() && packed.settings.instructions.trim().is_empty() && packed.settings.pinned.is_empty() && packed.ideas.is_none();
        Ok((!empty).then(|| render_markdown(&packed)))
    }

    /// `project_search` and `project_read` for every new session.
    pub fn host_tools(&self) -> (Vec<HostToolSpec>, Arc<dyn HostTools>) {
        let tools = crate::tools::tool_set(Arc::new(self.sessions.clone()), self.access.clone());
        (tools.specs(), Arc::new(tools))
    }
}
