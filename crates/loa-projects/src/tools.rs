//! `project_search` and `project_read` as [`Tool`]s, served to every harness through a [`ToolSet`].
//! The project is the calling session's.

use crate::read::{ReadRequest, read_subtree};
use crate::search::{AttrFilter, SearchQuery, TextMode, search};
use crate::source::{Access, SessionSource, session_project};
use crate::{Error, Result};
use async_trait::async_trait;
use loa_contracts::tools::{CallCtx, Tool, ToolOutput, ToolSet};
use loa_core::{ElementKind, NodeId};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

pub const PROJECT_SEARCH: &str = "project_search";
pub const PROJECT_READ: &str = "project_read";

/// Both tools over `source`, showing what `access` allows.
pub fn tool_set(source: Arc<dyn SessionSource>, access: Arc<dyn Access>) -> ToolSet {
    let scope = Scope { source, access };
    let mut set = ToolSet::new();
    set.register(ProjectSearch(scope.clone())).register(ProjectRead(scope));
    set
}

#[derive(Clone)]
struct Scope {
    source: Arc<dyn SessionSource>,
    access: Arc<dyn Access>,
}

impl Scope {
    async fn project_of(&self, session: &str) -> Result<String> {
        let dom = self.source.load(session).await?.ok_or_else(|| Error::SessionNotFound(session.to_owned()))?;
        session_project(&dom).ok_or_else(|| Error::Arguments("this session is not in a project".into()))
    }
}

pub struct ProjectSearch(Scope);
pub struct ProjectRead(Scope);

/// Search arguments.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    /// Text to find, case insensitive.
    query: Option<String>,
    /// phrase: the whole query as written. all_words: every word, anywhere.
    #[serde(default)]
    mode: TextMode,
    /// Only elements of this type (message, call, decision, todo, ...). The query then matches anywhere
    /// inside the element, e.g. element=call with a file path finds every call that touched it.
    element: Option<String>,
    /// Attributes the element must have. A string matches part of the value; null only requires the
    /// attribute, e.g. {"answer": null} on decisions.
    #[serde(default)]
    attributes: BTreeMap<String, Option<String>>,
    /// Only these session ids. List this chat's own id to search it too.
    sessions: Option<Vec<String>>,
    /// Also search sessions excluded from project context.
    #[serde(default)]
    include_excluded: bool,
    #[schemars(range(min = 1, max = 100))]
    limit: Option<usize>,
    /// next_cursor of the previous page.
    cursor: Option<String>,
}

/// Read arguments.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    /// Session id from a search hit or digest.
    session: String,
    /// Node id from a search hit or digest pointer. Omit for the whole session.
    #[schemars(range(min = 1))]
    node: Option<u64>,
    /// next_offset of the previous page.
    #[serde(default)]
    offset: usize,
    #[schemars(range(min = 1, max = 32000))]
    max_bytes: Option<usize>,
}

#[async_trait]
impl Tool for ProjectSearch {
    type Args = SearchArgs;
    const NAME: &'static str = PROJECT_SEARCH;

    fn description(&self) -> &str {
        "Search the other sessions of this project by text or element type (decisions, calls, messages, todos, ...). \
         Returns hits with session id, node id, a snippet and where the node sits. Read a hit with project_read."
    }

    async fn run(&self, ctx: &CallCtx, args: SearchArgs) -> Result<ToolOutput, String> {
        let caller = ctx.session_id.as_str();
        let run = async {
            let project = self.0.project_of(caller).await?;
            let kind = match args.element.as_deref() {
                Some(e) => Some(ElementKind::parse(e).ok_or_else(|| Error::Arguments(format!("unknown element `{e}`")))?),
                None => None,
            };
            // The caller's own session is left out unless `sessions` names it.
            let listed = args.sessions.as_ref().is_some_and(|only| only.iter().any(|s| s == caller));
            let query = SearchQuery {
                text: args.query,
                mode: args.mode,
                kind,
                attrs: args.attributes.into_iter().map(|(name, value)| AttrFilter { name, value }).collect(),
                skip_session: (!listed).then(|| caller.to_owned()),
                sessions: args.sessions,
                include_excluded: args.include_excluded,
                limit: args.limit.unwrap_or(0),
                cursor: args.cursor,
            };
            search(&*self.0.source, &*self.0.access, &project, &query).await
        };
        let page = run.await.map_err(|e| e.to_string())?;
        let hit = |h: &crate::search::SearchHit| json!({"session": h.session, "session_title": h.session_title, "node": h.node.0, "element": h.kind.as_str(), "snippet": h.snippet, "path": h.path});
        let hits: Vec<serde_json::Value> = page.hits.iter().map(hit).collect();
        Ok(ToolOutput::text(json!({"total": page.total, "hits": hits, "next_cursor": page.next_cursor}).to_string()))
    }
}

#[async_trait]
impl Tool for ProjectRead {
    type Args = ReadArgs;
    const NAME: &'static str = PROJECT_READ;

    fn description(&self) -> &str {
        "Read the full record behind a project_search hit or a digest pointer: the element and everything inside it, in pages."
    }

    async fn run(&self, ctx: &CallCtx, args: ReadArgs) -> Result<ToolOutput, String> {
        let request = ReadRequest { session: args.session, node: args.node.map(NodeId), offset: args.offset, max_bytes: args.max_bytes.unwrap_or(0) };
        let run = async {
            let project = self.0.project_of(&ctx.session_id).await?;
            read_subtree(&*self.0.source, &*self.0.access, &project, &request).await
        };
        let page = run.await.map_err(|e| e.to_string())?;
        let end = page.offset + page.text.len();
        let mut out = format!("session {} node {} (bytes {}..{end} of {})\n{}", page.session, page.node, page.offset, page.total_bytes, page.text);
        if let Some(next) = page.next_offset {
            out += &format!("\n[more: call project_read with offset={next}]\n");
        }
        Ok(ToolOutput::text(out))
    }
}
