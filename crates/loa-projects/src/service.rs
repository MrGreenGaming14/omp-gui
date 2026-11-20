//! The projects service `loa-app` holds: project listing for the sidebar,
//! the project screen's data and edits, digest rebuilds, the context of new
//! chats and the `project_search` / `project_read` host tools.

use crate::context::{ContextRequest, build_context, render_markdown};
use crate::digest::{DigestContent, DigestState, digest_of, digest_state};
use crate::digester::{Digester, PromptLimits, Summarizer};
use crate::rank::last_active;
use crate::rebuild::{RebuildDriver, RebuildHandle, RebuildOutcome, Trigger};
use crate::sessions::LiveSessions;
use crate::settings::{ProjectSettings, SettingsStore, exclude_draft, is_excluded};
use crate::source::{Access, Requester, SessionInfo, visible_sessions};
use crate::tools::ProjectTools;
use crate::{Error, Result};
use loa_adapter::{HostToolSpec, HostTools};
use loa_core::{Dom, Draft, ElementKind, attr};
use std::collections::BTreeMap;
use std::path::PathBuf;
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
    /// The folder when set, else the working directory of the latest session.
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

/// A project's folder as a path string, when one is set.
fn folder_of(settings: &ProjectSettings) -> Option<String> {
    settings.folder.as_ref().map(|dir| dir.to_string_lossy().into_owned())
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
        summarizer: Arc<dyn Summarizer>,
        config: ServiceConfig,
    ) -> (Self, mpsc::UnboundedReceiver<RebuildOutcome>) {
        let digester = Digester::new(summarizer, config.limits);
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

    /// Every project: each stored project and each project a session names,
    /// most recently active first. A project's folder is its stored one when
    /// set, else the cwd of its most recent session.
    pub async fn projects(&self) -> Result<Vec<ProjectSummary>> {
        let mut folders = BTreeMap::new();
        let mut projects: BTreeMap<String, (ProjectSummary, i64)> = BTreeMap::new();
        for name in self.settings.names().await? {
            let settings = self.settings.load(&name).await?;
            if let Some(folder) = folder_of(&settings) {
                folders.insert(name.clone(), folder);
            }
            let summary = ProjectSummary { name: name.clone(), cwd: None, sessions: 0, last_active: 0 };
            projects.insert(name, (summary, i64::MIN));
        }
        for (info, dom) in self.sessions.all().await? {
            if info.project.is_empty() {
                continue;
            }
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

    /// Creates a project, or sets the folder of an existing one when `folder`
    /// is given. Fails on a blank name.
    pub async fn create(&self, name: &str, folder: Option<PathBuf>) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::EmptyName);
        }
        let mut settings = self.settings.load(name).await?;
        if folder.is_some() {
            settings.folder = folder;
        }
        self.settings.save(name, &settings).await
    }

    /// Puts `session` into `project`, or out of every project when `None`. A
    /// blank name counts as `None`.
    pub async fn move_session(&self, session: &str, project: Option<&str>) -> Result<()> {
        let project = project.map(str::trim).filter(|name| !name.is_empty());
        let dom = crate::source::SessionSource::load(&self.sessions, session)
            .await?
            .ok_or_else(|| Error::SessionNotFound(session.to_owned()))?;
        let root = dom.root().ok_or_else(|| Error::SessionNotFound(session.to_owned()))?;
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

    /// The project screen's data.
    pub async fn overview(&self, project: &str) -> Result<ProjectOverview> {
        let settings = self.settings.load(project).await?;
        let mut sessions = Vec::new();
        let mut cwd: Option<(i64, String)> = None;
        for info in visible_sessions(&self.sessions, &*self.access, &Requester::Local, project).await? {
            let Some(dom) = crate::source::SessionSource::load(&self.sessions, &info.id).await? else { continue };
            let row = SessionRow::of(&info, &dom);
            if let Some(dir) = dom.root().and_then(|r| r.str_attr(attr::CWD))
                && cwd.as_ref().is_none_or(|(at, _)| row.last_active >= *at)
            {
                cwd = Some((row.last_active, dir.to_owned()));
            }
            sessions.push(row);
        }
        sessions.sort_by(|a, b| b.last_active.cmp(&a.last_active).then_with(|| a.id.cmp(&b.id)));
        let folder = folder_of(&settings);
        let cwd = folder.or_else(|| cwd.map(|(_, dir)| dir));
        Ok(ProjectOverview { name: project.to_owned(), cwd, settings, sessions })
    }

    pub async fn save_settings(&self, project: &str, settings: &ProjectSettings) -> Result<()> {
        self.settings.save(project, settings).await
    }

    /// Sets the session's exclude toggle. Including a session again queues
    /// a rebuild, since excluded sessions don't get digests. Returns whether
    /// anything changed.
    pub async fn set_excluded(&self, session: &str, excluded: bool) -> Result<bool> {
        let dom = crate::source::SessionSource::load(&self.sessions, session)
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
            requester: &Requester::Local,
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
        let tools = ProjectTools::new(Arc::new(self.sessions.clone()), self.access.clone(), Requester::Local);
        (ProjectTools::specs(), Arc::new(tools))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digester::SummaryAnswer;
    use crate::settings::{FileSettingsStore, PinnedFile};
    use crate::source::LocalOnly;
    use async_trait::async_trait;
    use loa_bus::{Authorize, Bus, Denied, NodeKey, Persist, PersistError, SessionId, SessionWriter, WriterConfig};
    use loa_core::{Draft, NodeId, Patch, PatchSink, Value};
    use loa_journal::{Journal, JournalOptions, Store};

    struct JournalPersist(Journal);

    impl Persist for JournalPersist {
        fn append(&mut self, patch: &Patch) -> Result<(), PersistError> {
            self.0.append(patch)?;
            Ok(self.0.sync()?)
        }
    }

    struct AllowAll;

    impl Authorize for AllowAll {
        fn authorize(&self, _: &SessionId, _: &NodeKey, _: &Draft, _: &Dom) -> Result<(), Denied> {
            Ok(())
        }
    }

    struct Fake;

    #[async_trait]
    impl Summarizer for Fake {
        async fn summarize(&self, prompt: &str, _: Option<&str>) -> Result<SummaryAnswer> {
            let summary = if prompt.contains("iroh") { "Set up iroh on port 31415." } else { "Other work." };
            let text = format!(
                r#"{{"summary":"{summary}","decisions":["port 31415"],"files":[{{"path":"src/net.rs","note":"endpoint"}}],"open_items":[],"pointers":["T1"]}}"#
            );
            Ok(SummaryAnswer { text, model: Some("smol".into()) })
        }
    }

    struct Env {
        _dir: tempfile::TempDir,
        bus: Arc<Bus>,
        store: Store,
        service: ProjectService,
        outcomes: mpsc::UnboundedReceiver<RebuildOutcome>,
    }

    fn env() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("sessions"), JournalOptions::default()).unwrap();
        let bus = Arc::new(Bus::new());
        let settings = Arc::new(FileSettingsStore::new(dir.path().join("projects")));
        let config = ServiceConfig { debounce: Duration::from_millis(10), ..ServiceConfig::default() };
        let (service, outcomes) = ProjectService::start(
            LiveSessions::new(bus.clone(), store.clone()),
            settings,
            Arc::new(LocalOnly),
            Arc::new(Fake),
            config,
        );
        Env { _dir: dir, bus, store, service, outcomes }
    }

    fn open(env: &Env, id: &str, project: &str, cwd: &str, prompt: &str, at: i64) -> Arc<SessionWriter> {
        let writer = env
            .bus
            .open(WriterConfig {
                session: SessionId::new(id),
                owner: NodeKey([1; 32]),
                local: NodeKey([1; 32]),
                dom: Dom::new(),
                persist: Box::new(JournalPersist(env.store.create(id).unwrap())),
                authorize: Arc::new(AllowAll),
            })
            .unwrap();
        let root = writer
            .commit_at(
                Draft::insert(
                    NodeId::DOCUMENT,
                    ElementKind::Session,
                    [
                        (attr::ID, Value::from(id)),
                        (attr::HARNESS, "omp".into()),
                        (attr::NODE, "n".into()),
                        (attr::CREATED, at.into()),
                        (attr::CWD, cwd.into()),
                        (attr::PROJECT, project.into()),
                        (attr::TITLE, format!("Title {id}").into()),
                    ],
                    "",
                ),
                at,
            )
            .unwrap()
            .node;
        let turn = writer
            .commit(Draft::insert(root, ElementKind::Turn, [(attr::STATUS, Value::from("done")), (attr::ENDED, at.into())], ""))
            .unwrap()
            .node;
        writer.commit(Draft::insert(turn, ElementKind::Message, [(attr::ROLE, Value::from("user"))], prompt)).unwrap();
        writer
    }

    #[tokio::test]
    async fn projects_overview_digest_and_context_end_to_end() {
        let mut env = env();
        let _a = open(&env, "a", "loa", "/home/u/loa", "set up iroh", 10);
        let _b = open(&env, "b", "loa", "/home/u/loa", "write docs", 20);
        let _c = open(&env, "c", "web", "/home/u/web", "landing page", 5);
        // Closed sessions stay in their project.
        env.bus.close("c");
        drop(_c);

        let projects = env.service.projects().await.unwrap();
        let names: Vec<(&str, usize)> = projects.iter().map(|p| (p.name.as_str(), p.sessions)).collect();
        assert_eq!(names, [("loa", 2), ("web", 1)]);
        assert_eq!(projects[0].cwd.as_deref(), Some("/home/u/loa"));

        // Turn end rebuilds the digest through the summarizer.
        assert!(env.service.notify("a", Trigger::TurnEnd));
        assert!(matches!(env.outcomes.recv().await.unwrap(), RebuildOutcome::Rebuilt { .. }));
        let overview = env.service.overview("loa").await.unwrap();
        let ids: Vec<&str> = overview.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
        let a = &overview.sessions[1];
        assert_eq!(a.digest_state, DigestState::Fresh);
        assert_eq!(a.digest.as_ref().unwrap().summary, "Set up iroh on port 31415.");
        assert_eq!(overview.sessions[0].digest_state, DigestState::Missing);

        // Shared instructions and pins persist and reach new chats with the digest.
        let settings = ProjectSettings {
            instructions: "Use rustls.".into(),
            pinned: vec![PinnedFile { path: "docs/ARCH.md".into(), note: None }],
            digest_model: None,
            folder: None,
        };
        env.service.save_settings("loa", &settings).await.unwrap();
        assert_eq!(env.service.overview("loa").await.unwrap().settings, settings);
        let context = env.service.context("loa", None, "iroh").await.unwrap().unwrap();
        assert!(context.contains("Use rustls."));
        assert!(context.contains("`docs/ARCH.md`"));
        assert!(context.contains("Set up iroh on port 31415."));
        assert_eq!(env.service.context("empty", None, "").await.unwrap(), None);

        // Excluding drops the session from context; the closed one is written in its journal.
        assert!(env.service.set_excluded("a", true).await.unwrap());
        assert!(!env.service.set_excluded("a", true).await.unwrap());
        let context = env.service.context("loa", None, "iroh").await.unwrap().unwrap();
        assert!(!context.contains("Set up iroh"));
        assert!(env.service.set_excluded("c", true).await.unwrap());
        assert!(env.service.overview("web").await.unwrap().sessions[0].excluded);

        // project_search finds the other session through the host tool.
        let (specs, handler) = env.service.host_tools();
        assert_eq!(specs.len(), 2);
        let out = handler.call("b", crate::PROJECT_SEARCH, r#"{"query":"iroh","include_excluded":true}"#).await.unwrap();
        let out: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(out["hits"][0]["session"], "a");
    }

    #[tokio::test]
    async fn sessions_without_a_project_belong_to_no_project() {
        let env = env();
        let _a = open(&env, "a", "loa", "/home/u/loa", "set up iroh", 10);
        let _b = open(&env, "b", "loa", "/home/u/scratch", "scratch", 20);
        env.service.move_session("b", None).await.unwrap();
        let projects = env.service.projects().await.unwrap();
        let rows: Vec<(&str, usize, Option<&str>)> =
            projects.iter().map(|p| (p.name.as_str(), p.sessions, p.cwd.as_deref())).collect();
        assert_eq!(rows, [("loa", 1, Some("/home/u/loa"))]);
    }

    #[tokio::test]
    async fn created_projects_list_with_their_folder() {
        let env = env();
        let _a = open(&env, "a", "loa", "/home/u/loa", "set up iroh", 10);
        env.service.create("  docs  ", Some(PathBuf::from("/home/u/docs"))).await.unwrap();
        env.service.create("empty", None).await.unwrap();
        env.service.create("loa", Some(PathBuf::from("/srv/loa"))).await.unwrap();
        let projects = env.service.projects().await.unwrap();
        let rows: Vec<(&str, usize, Option<&str>)> =
            projects.iter().map(|p| (p.name.as_str(), p.sessions, p.cwd.as_deref())).collect();
        assert_eq!(rows, [("loa", 1, Some("/srv/loa")), ("docs", 0, Some("/home/u/docs")), ("empty", 0, None)]);
    }

    #[tokio::test]
    async fn create_rejects_a_blank_name() {
        let env = env();
        assert!(matches!(env.service.create("  ", None).await, Err(Error::EmptyName)));
        assert!(env.service.projects().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn move_session_sets_and_clears_the_project() {
        let env = env();
        let _a = open(&env, "a", "loa", "/home/u/loa", "set up iroh", 10);
        env.service.move_session("a", Some("web")).await.unwrap();
        let moved = env.service.projects().await.unwrap();
        let names: Vec<(&str, usize)> = moved.iter().map(|p| (p.name.as_str(), p.sessions)).collect();
        assert_eq!(names, [("web", 1)]);
        env.service.move_session("a", None).await.unwrap();
        assert!(env.service.projects().await.unwrap().is_empty());
        assert!(matches!(env.service.move_session("nope", Some("web")).await, Err(Error::SessionNotFound(_))));
    }
}
