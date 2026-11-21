//! The projects service `loa-app` holds: listing, the project screen, rebuilds, chat context and tools.

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
use loa_core::{Dom, ElementKind, attr};
use loa_settings::projects::{ProjectOverview, ProjectSettings, SessionRow};
use std::collections::BTreeMap;
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

/// A project as the sidebar lists it: `cwd` of its latest session, `last_active` in Unix ms.
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

    /// Every project that has sessions, most recently active first.
    pub async fn projects(&self) -> Result<Vec<ProjectSummary>> {
        let mut projects: BTreeMap<String, (ProjectSummary, i64)> = BTreeMap::new();
        for (info, dom) in self.sessions.all().await? {
            let active = last_active(&dom);
            let cwd = dom.root().and_then(|r| r.str_attr(attr::CWD)).map(str::to_owned);
            let fresh = || (ProjectSummary { name: info.project.clone(), cwd: None, sessions: 0, last_active: active }, i64::MIN);
            let (summary, cwd_at) = projects.entry(info.project.clone()).or_insert_with(fresh);
            summary.sessions += 1;
            summary.last_active = summary.last_active.max(active);
            if cwd.is_some() && active >= *cwd_at {
                summary.cwd = cwd;
                *cwd_at = active;
            }
        }
        let mut projects: Vec<ProjectSummary> = projects.into_values().map(|(p, _)| p).collect();
        projects.sort_by(|a, b| b.last_active.cmp(&a.last_active).then_with(|| a.name.cmp(&b.name)));
        Ok(projects)
    }

    pub async fn overview(&self, project: &str) -> Result<ProjectOverview> {
        let settings = self.settings.load(project).await?;
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
        Ok(ProjectOverview { name: project.to_owned(), cwd: cwd.map(|(_, dir)| dir), settings, sessions })
    }

    pub async fn save_settings(&self, project: &str, settings: &ProjectSettings) -> Result<()> {
        self.settings.save(project, settings).await
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
        let empty = packed.digests.is_empty() && packed.settings.instructions.trim().is_empty() && packed.settings.pinned.is_empty();
        Ok((!empty).then(|| render_markdown(&packed)))
    }

    /// `project_search` and `project_read` for every new session.
    pub fn host_tools(&self) -> (Vec<HostToolSpec>, Arc<dyn HostTools>) {
        let tools = crate::tools::tool_set(Arc::new(self.sessions.clone()), self.access.clone());
        (tools.specs(), Arc::new(tools))
    }
}
