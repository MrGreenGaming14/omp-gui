//! Ports for reading a project's sessions and checking which may be shown.

use crate::{Error, Result};
use async_trait::async_trait;
use loa_core::{Dom, ElementKind, Role, attr};
use std::sync::Arc;

/// What a project listing knows about a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub project: String,
    /// Owner node key.
    pub node: String,
    /// Unix milliseconds.
    pub created: i64,
    /// The `<session title>`, else the first line of the first user prompt.
    pub title: Option<String>,
}

impl SessionInfo {
    /// `None` without a `<session id>` or a project.
    pub fn from_dom(dom: &Dom) -> Option<Self> {
        let root = dom.root()?;
        let title = root.str_attr(attr::TITLE).map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned).or_else(|| {
            let user = Some(Role::User.as_str());
            let prompt = dom.iter().find(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == user)?;
            Some(loa_util::text::first_line(&prompt.text, 60)).filter(|t| !t.is_empty())
        });
        let node = root.str_attr(attr::NODE).unwrap_or_default().to_owned();
        Some(Self { id: root.str_attr(attr::ID)?.to_owned(), project: session_project(dom)?, node, created: root.int_attr(attr::CREATED).unwrap_or(0), title })
    }
}

/// The session's `<session project>`, trimmed. A session without one belongs to no project; its working directory
/// does not name one.
pub fn session_project(dom: &Dom) -> Option<String> {
    let project = dom.root()?.str_attr(attr::PROJECT)?.trim();
    (!project.is_empty()).then(|| project.to_owned())
}

/// Loads the sessions of a project.
#[async_trait]
pub trait SessionSource: Send + Sync {
    /// Every session of `project`, in any order.
    async fn list(&self, project: &str) -> Result<Vec<SessionInfo>>;
    async fn load(&self, session: &str) -> Result<Option<Arc<Dom>>>;
}

/// Which sessions this node's user may see through projects.
pub trait Access: Send + Sync {
    fn can_open(&self, session: &SessionInfo) -> bool;
}

/// Every local session is visible.
#[derive(Copy, Clone, Debug, Default)]
pub struct LocalOnly;

impl Access for LocalOnly {
    fn can_open(&self, _: &SessionInfo) -> bool { true }
}

/// The visible sessions of `project`, newest first (ties by id).
pub async fn visible_sessions(source: &dyn SessionSource, access: &dyn Access, project: &str) -> Result<Vec<SessionInfo>> {
    let mut sessions = source.list(project).await?;
    sessions.retain(|s| s.project == project && access.can_open(s));
    sessions.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| a.id.cmp(&b.id)));
    Ok(sessions)
}

/// The visible session `id` of `project`; [`Error::SessionNotFound`] when missing, elsewhere or denied.
pub(crate) async fn open_visible(source: &dyn SessionSource, access: &dyn Access, project: &str, id: &str) -> Result<Arc<Dom>> {
    let not_found = || Error::SessionNotFound(id.to_owned());
    visible_sessions(source, access, project).await?.into_iter().find(|s| s.id == id).ok_or_else(not_found)?;
    source.load(id).await?.ok_or_else(not_found)
}
