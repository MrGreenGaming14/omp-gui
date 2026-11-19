//! Ports for reading a project's sessions and checking who may open them.

use crate::Result;
use async_trait::async_trait;
use loa_core::{Dom, ElementKind, Role, attr};
use std::path::Path;
use std::sync::Arc;

/// Who is asking: this node's own user, or a peer identified by its node key.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Requester {
    Local,
    Peer(String),
}

/// What a project listing knows about a session without loading its DOM.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    /// App-wide session id, as in `<session id>`.
    pub id: String,
    /// Project id: `<session project>`, else the last component of
    /// `<session cwd>` (see [`session_project`]).
    pub project: String,
    /// Owner node key, as in `<session node>`.
    pub node: String,
    /// Unix milliseconds, as in `<session created>`.
    pub created: i64,
    /// The `<session title>`, else the first line of the first user prompt
    /// (clipped), the title Recents shows.
    pub title: Option<String>,
}

impl SessionInfo {
    /// The listing entry of a session DOM. `None` without a `<session id>`
    /// or a project (see [`session_project`]).
    pub fn from_dom(dom: &Dom) -> Option<Self> {
        let root = dom.root()?;
        Some(Self {
            id: root.str_attr(attr::ID)?.to_owned(),
            project: session_project(dom)?,
            node: root.str_attr(attr::NODE).unwrap_or_default().to_owned(),
            created: root.int_attr(attr::CREATED).unwrap_or(0),
            title: session_title(dom),
        })
    }
}

/// Characters of a prompt line kept in a title derived from it.
const DERIVED_TITLE_CHARS: usize = 60;

/// The `<session title>`, else the first line of the first user prompt.
fn session_title(dom: &Dom) -> Option<String> {
    let root = dom.root()?;
    if let Some(title) = root.str_attr(attr::TITLE).map(str::trim).filter(|title| !title.is_empty()) {
        return Some(title.to_owned());
    }
    let prompt = dom.iter().find(|node| node.kind == ElementKind::Message && node.str_attr(attr::ROLE) == Some(Role::User.as_str()))?;
    let line = prompt.text.lines().map(str::trim).find(|line| !line.is_empty())?;
    Some(match line.char_indices().nth(DERIVED_TITLE_CHARS) {
        Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
        None => line.to_owned(),
    })
}

/// The project a session belongs to: its `<session project>`, else the name
/// of its working directory (`<session cwd>`), the same name the app gives
/// a project it starts in that directory.
pub fn session_project(dom: &Dom) -> Option<String> {
    let root = dom.root()?;
    if let Some(project) = root.str_attr(attr::PROJECT).filter(|p| !p.trim().is_empty()) {
        return Some(project.to_owned());
    }
    root.str_attr(attr::CWD).and_then(project_name)
}

/// The project name of a working directory: its last component, or the whole
/// path for a root like `/`.
pub fn project_name(cwd: &str) -> Option<String> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return None;
    }
    let path = Path::new(cwd);
    Some(path.file_name().map_or_else(|| cwd.to_owned(), |name| name.to_string_lossy().into_owned()))
}

/// Loads the sessions of a project. `loa-app` backs it with the journal.
#[async_trait]
pub trait SessionSource: Send + Sync {
    /// Every session of `project`, in any order.
    async fn list(&self, project: &str) -> Result<Vec<SessionInfo>>;

    /// The current DOM of a session. `None` when it doesn't exist.
    async fn load(&self, session: &str) -> Result<Option<Arc<Dom>>>;
}

/// The access check. `loa-app` backs it with `loa-grants`, using the same
/// check as opening the session directly. A "future sessions only" grant
/// answers `false` for sessions created before the grant.
pub trait Access: Send + Sync {
    fn can_open(&self, requester: &Requester, session: &SessionInfo) -> bool;
}

/// Access for this node's own user only: every session opens locally, no
/// peer opens any. For an app without peer grants wired in.
#[derive(Copy, Clone, Debug, Default)]
pub struct LocalOnly;

impl Access for LocalOnly {
    fn can_open(&self, requester: &Requester, _: &SessionInfo) -> bool {
        *requester == Requester::Local
    }
}

/// The sessions of `project` that `requester` may open, newest first (ties
/// by id). Every search, read and context build starts here, so a session
/// that fails the check is dropped before anything looks at its content.
pub async fn visible_sessions(
    source: &dyn SessionSource,
    access: &dyn Access,
    requester: &Requester,
    project: &str,
) -> Result<Vec<SessionInfo>> {
    let mut sessions = source.list(project).await?;
    sessions.retain(|s| s.project == project && access.can_open(requester, s));
    sessions.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)));
    Ok(sessions)
}

/// The visible session `id` of `project`, or [`Error::SessionNotFound`]
/// whether it is missing, elsewhere or denied.
///
/// [`Error::SessionNotFound`]: crate::Error::SessionNotFound
pub(crate) async fn open_visible(
    source: &dyn SessionSource,
    access: &dyn Access,
    requester: &Requester,
    project: &str,
    id: &str,
) -> Result<(SessionInfo, Arc<Dom>)> {
    let not_found = || crate::Error::SessionNotFound(id.to_owned());
    let info = visible_sessions(source, access, requester, project)
        .await?
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(not_found)?;
    let dom = source.load(id).await?.ok_or_else(not_found)?;
    Ok((info, dom))
}

#[cfg(test)]
pub(crate) mod testing {
    //! In-memory ports and DOM builders for tests.

    use super::*;
    use loa_core::{Draft, ElementKind as K, MemorySink, NodeId, PatchSink, Value, attr};
    use std::collections::{BTreeMap, HashSet};
    use std::sync::Mutex;

    /// A session DOM under construction.
    pub struct Builder {
        pub sink: MemorySink,
        pub root: NodeId,
    }

    impl Builder {
        pub fn new(id: &str, project: &str, created: i64) -> Self {
            let sink = MemorySink::new();
            let root = sink
                .commit_at(
                    Draft::insert(
                        NodeId::DOCUMENT,
                        K::Session,
                        [
                            (attr::ID, Value::from(id)),
                            (attr::HARNESS, "omp".into()),
                            (attr::NODE, "node-a".into()),
                            (attr::CREATED, created.into()),
                            (attr::PROJECT, project.into()),
                        ],
                        "",
                    ),
                    created,
                )
                .unwrap()
                .node;
            Self { sink, root }
        }

        pub fn commit(&self, draft: Draft) -> NodeId {
            self.sink.commit_at(draft, 0).unwrap().node
        }

        /// A finished turn with a user prompt and an assistant reply.
        pub fn turn(&self, user: &str, assistant: &str, ended: i64) -> NodeId {
            let turn = self.commit(Draft::insert(
                self.root,
                K::Turn,
                [(attr::STATUS, Value::from("done")), (attr::ENDED, ended.into())],
                "",
            ));
            self.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("user"))], user));
            self.commit(Draft::insert(turn, K::Message, [(attr::ROLE, Value::from("assistant"))], assistant));
            turn
        }

        /// A tool call under `turn` with JSON `input` and a result.
        pub fn call(&self, turn: NodeId, tool: &str, input: &str, result: &str) -> NodeId {
            let call = self.commit(Draft::insert(turn, K::Call, [(attr::TOOL, Value::from(tool))], ""));
            self.commit(Draft::insert(call, K::Input, [] as [(&str, Value); 0], input));
            self.commit(Draft::insert(call, K::Result, [] as [(&str, Value); 0], result));
            call
        }

        pub fn dom(&self) -> Dom {
            self.sink.dom()
        }
    }

    pub fn info_of(dom: &Dom) -> SessionInfo {
        let root = dom.root().unwrap();
        SessionInfo {
            id: root.str_attr(attr::ID).unwrap().to_owned(),
            project: root.str_attr(attr::PROJECT).unwrap_or_default().to_owned(),
            node: root.str_attr(attr::NODE).unwrap().to_owned(),
            created: root.int_attr(attr::CREATED).unwrap(),
            title: root.str_attr(attr::TITLE).map(str::to_owned),
        }
    }

    /// Sessions held in memory.
    #[derive(Default)]
    pub struct MemorySource {
        pub doms: Mutex<BTreeMap<String, Arc<Dom>>>,
    }

    impl MemorySource {
        pub fn with(doms: impl IntoIterator<Item = Dom>) -> Self {
            let source = Self::default();
            for dom in doms {
                source.put(dom);
            }
            source
        }

        pub fn put(&self, dom: Dom) {
            let id = info_of(&dom).id;
            self.doms.lock().unwrap().insert(id, Arc::new(dom));
        }
    }

    #[async_trait]
    impl SessionSource for MemorySource {
        async fn list(&self, project: &str) -> Result<Vec<SessionInfo>> {
            Ok(self.doms.lock().unwrap().values().map(|d| info_of(d)).filter(|i| i.project == project).collect())
        }

        async fn load(&self, session: &str) -> Result<Option<Arc<Dom>>> {
            Ok(self.doms.lock().unwrap().get(session).cloned())
        }
    }

    /// Allows everything to `Local`; peers get the listed sessions, plus
    /// every session created at or after `future_since` when set.
    #[derive(Default)]
    pub struct TestAccess {
        pub allowed: HashSet<String>,
        pub future_since: Option<i64>,
    }

    impl TestAccess {
        pub fn only(ids: &[&str]) -> Self {
            Self { allowed: ids.iter().map(|s| (*s).to_owned()).collect(), future_since: None }
        }

        pub fn future_since(at: i64) -> Self {
            Self { allowed: HashSet::new(), future_since: Some(at) }
        }
    }

    impl Access for TestAccess {
        fn can_open(&self, requester: &Requester, session: &SessionInfo) -> bool {
            match requester {
                Requester::Local => true,
                Requester::Peer(_) => {
                    self.allowed.contains(&session.id) || self.future_since.is_some_and(|t| session.created >= t)
                }
            }
        }
    }

    pub fn peer() -> Requester {
        Requester::Peer("peer-key".to_owned())
    }
}
