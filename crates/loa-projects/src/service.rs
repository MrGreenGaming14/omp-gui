//! The projects service `loa-app` holds: project listing for the sidebar,
//! the project screen's data and edits, digest rebuilds, the context of new
//! chats and the `project_search` / `project_read` host tools.

use crate::context::{ContextRequest, build_context, render_markdown};
use crate::digest::{DigestContent, DigestState, digest_of, digest_state};
use crate::digester::{Digester, PromptLimits, Summarizers};
use crate::rank::last_active;
use crate::rebuild::{RebuildDriver, RebuildHandle, RebuildOutcome, Trigger};
use crate::sessions::LiveSessions;
use crate::settings::{ProjectSettings, SettingsStore, exclude_draft, is_excluded};
use crate::source::{Access, SessionInfo, SessionSource, visible_sessions};
use crate::tools::tool_set;
use crate::{Error, Result};
use loa_contracts::harness::{HostToolSpec, HostTools};
use loa_core::{Dom, ElementKind, attr};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Tunables of the service.
#[derive(Clone, Debug)]
pub struct ServiceConfig {
    /// Quiet time after a turn end or compaction before a session's digest
    /// is rebuilt, so a burst of turns costs one model call.
    pub debounce: Duration,
    /// Token budget of the context a new chat receives.
    pub context_budget: usize,
    pub limits: PromptLimits,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self { debounce: Duration::from_secs(10), context_budget: 4_000, limits: PromptLimits::default() }
    }
}

/// A project as the sidebar lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectSummary {
    pub name: String,
    /// Working directory of the project's most recent session.
    pub cwd: Option<String>,
    pub sessions: usize,
    /// Unix milliseconds of the latest activity in any of its sessions.
    pub last_active: i64,
}

/// One session on the project screen.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub harness: Option<String>,
    pub created: i64,
    pub last_active: i64,
    pub turns: usize,
    /// Left out of project context by the user.
    pub excluded: bool,
    pub digest_state: DigestState,
    pub digest: Option<DigestContent>,
    /// Model or role that wrote the digest.
    pub digest_model: Option<String>,
}

impl SessionRow {
    fn of(info: &SessionInfo, dom: &Dom) -> Self {
        let root = dom.root();
        let digest = digest_of(dom);
        Self {
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
}

/// Everything the project screen shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ProjectOverview {
    pub name: String,
    pub cwd: Option<String>,
    pub settings: ProjectSettings,
    /// Newest activity first.
    pub sessions: Vec<SessionRow>,
}

/// The projects service. Cheap to clone; every clone feeds the same rebuild
/// driver.
#[derive(Clone)]
pub struct ProjectService {
    sessions: LiveSessions,
    settings: Arc<dyn SettingsStore>,
    access: Arc<dyn Access>,
    rebuild: RebuildHandle,
    context_budget: usize,
}

impl ProjectService {
    /// Starts the service and its digest rebuild driver on the current tokio
    /// runtime. The receiver reports every rebuild; the driver stops when
    /// the last clone of the service is dropped.
    pub fn start(
        sessions: LiveSessions,
        settings: Arc<dyn SettingsStore>,
        access: Arc<dyn Access>,
        summarizers: Arc<dyn Summarizers>,
        config: ServiceConfig,
    ) -> (Self, mpsc::UnboundedReceiver<RebuildOutcome>) {
        let digester = Digester::new(summarizers, config.limits);
        let source = Arc::new(sessions.clone());
        let (rebuild, outcomes) = RebuildDriver::new(source.clone(), source, digester, config.debounce)
            .with_settings(settings.clone())
            .spawn();
        (Self { sessions, settings, access, rebuild, context_budget: config.context_budget }, outcomes)
    }

    pub fn sessions(&self) -> &LiveSessions {
        &self.sessions
    }

    /// Reports a turn end or compaction in `session`; its digest is rebuilt
    /// once the session goes quiet, if it is missing or stale.
    pub fn notify(&self, session: &str, trigger: Trigger) -> bool {
        self.rebuild.notify(session, trigger)
    }

    /// Every project that has sessions, most recently active first.
    pub async fn projects(&self) -> Result<Vec<ProjectSummary>> {
        let mut projects: BTreeMap<String, (ProjectSummary, i64)> = BTreeMap::new();
        for (info, dom) in self.sessions.all().await? {
            let active = last_active(&dom);
            let cwd = dom.root().and_then(|r| r.str_attr(attr::CWD)).map(str::to_owned);
            let (summary, cwd_at) = projects.entry(info.project.clone()).or_insert_with(|| {
                (ProjectSummary { name: info.project.clone(), cwd: None, sessions: 0, last_active: active }, i64::MIN)
            });
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

    /// The project screen's data.
    pub async fn overview(&self, project: &str) -> Result<ProjectOverview> {
        let settings = self.settings.load(project).await?;
        let mut sessions = Vec::new();
        let mut cwd: Option<(i64, String)> = None;
        for info in visible_sessions(&self.sessions, &*self.access, project).await? {
            let Some(dom) = SessionSource::load(&self.sessions, &info.id).await? else { continue };
            let row = SessionRow::of(&info, &dom);
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

    /// Sets the session's exclude toggle. Including a session again queues
    /// a rebuild, since excluded sessions don't get digests. Returns whether
    /// anything changed.
    pub async fn set_excluded(&self, session: &str, excluded: bool) -> Result<bool> {
        let dom = SessionSource::load(&self.sessions, session)
            .await?
            .ok_or_else(|| Error::SessionNotFound(session.to_owned()))?;
        let Some(draft) = exclude_draft(&dom, excluded) else { return Ok(false) };
        self.sessions.commit(session, draft).await?;
        if !excluded {
            self.notify(session, Trigger::TurnEnd);
        }
        Ok(true)
    }

    /// The project context for a new chat in `project`: shared instructions,
    /// pinned files and the best digests of the other sessions within the
    /// token budget, as markdown for `SessionConfig::context`. `query` is the
    /// chat's first prompt or title, empty when unknown (ranks by recency).
    /// `None` when the project has nothing to share yet.
    pub async fn context(&self, project: &str, session: Option<&str>, query: &str) -> Result<Option<String>> {
        let request = ContextRequest {
            project,
            session,
            query,
            budget_tokens: self.context_budget,
            now: loa_core::now_ms(),
        };
        let packed = build_context(&self.sessions, &*self.access, &*self.settings, &request).await?;
        let empty = packed.digests.is_empty()
            && packed.settings.instructions.trim().is_empty()
            && packed.settings.pinned.is_empty();
        Ok((!empty).then(|| render_markdown(&packed)))
    }

    /// `project_search` and `project_read` for every new session, with the
    /// handler that serves them for this node's user.
    pub fn host_tools(&self) -> (Vec<HostToolSpec>, Arc<dyn HostTools>) {
        let tools = tool_set(Arc::new(self.sessions.clone()), self.access.clone());
        (tools.specs(), Arc::new(tools))
    }
}
