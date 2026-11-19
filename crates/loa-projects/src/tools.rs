//! `project_search` and `project_read` as host tools served to every harness.

use crate::read::{ReadPage, ReadRequest, read_subtree};
use crate::search::{AttrFilter, SearchPage, SearchQuery, TextMode, search};
use crate::source::{Access, Requester, SessionSource, session_project};
use crate::{Error, Result};
use async_trait::async_trait;
use loa_adapter::{HostToolSpec, HostTools};
use loa_core::{ElementKind, NodeId};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;

pub const PROJECT_SEARCH: &str = "project_search";
pub const PROJECT_READ: &str = "project_read";

/// Serves both tools for the sessions of one requester. The project is the
/// calling session's `<session project>`.
pub struct ProjectTools {
    source: Arc<dyn SessionSource>,
    access: Arc<dyn Access>,
    requester: Requester,
}

impl ProjectTools {
    pub fn new(source: Arc<dyn SessionSource>, access: Arc<dyn Access>, requester: Requester) -> Self {
        Self { source, access, requester }
    }

    /// Specs to put in `SessionConfig::host_tools`.
    pub fn specs() -> Vec<HostToolSpec> {
        let elements: Vec<&str> = ElementKind::VALUES.to_vec();
        let search = json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Text to find, case insensitive."},
                "mode": {"type": "string", "enum": ["phrase", "all_words"], "default": "phrase",
                         "description": "phrase: the whole query as written. all_words: every word, anywhere."},
                "element": {"type": "string", "enum": elements,
                            "description": "Only elements of this type. The query then matches anywhere inside the element, e.g. element=call with a file path finds every call that touched it."},
                "attributes": {"type": "object", "additionalProperties": {"type": ["string", "null"]},
                               "description": "Attributes the element must have. A string matches part of the value; null only requires the attribute, e.g. {\"answer\": null} on decisions."},
                "sessions": {"type": "array", "items": {"type": "string"}, "description": "Only these session ids. List this chat's own id to search it too."},
                "include_excluded": {"type": "boolean", "default": false,
                                     "description": "Also search sessions excluded from project context."},
                "limit": {"type": "integer", "minimum": 1, "maximum": crate::search::MAX_LIMIT, "default": crate::search::DEFAULT_LIMIT},
                "cursor": {"type": "string", "description": "next_cursor of the previous page."}
            },
            "additionalProperties": false
        });
        let read = json!({
            "type": "object",
            "properties": {
                "session": {"type": "string", "description": "Session id from a search hit or digest."},
                "node": {"type": "integer", "minimum": 1, "description": "Node id from a search hit or digest pointer. Omit for the whole session."},
                "offset": {"type": "integer", "minimum": 0, "default": 0, "description": "next_offset of the previous page."},
                "max_bytes": {"type": "integer", "minimum": 1, "maximum": crate::read::MAX_PAGE_BYTES, "default": crate::read::DEFAULT_PAGE_BYTES}
            },
            "required": ["session"],
            "additionalProperties": false
        });
        vec![
            HostToolSpec {
                name: PROJECT_SEARCH.to_owned(),
                description: "Search the other sessions of this project by text or element type (decisions, calls, \
                              messages, todos, ...). Returns hits with session id, node id, a snippet and where \
                              the node sits. Read a hit with project_read."
                    .to_owned(),
                input_schema: search.to_string(),
            },
            HostToolSpec {
                name: PROJECT_READ.to_owned(),
                description: "Read the full record behind a project_search hit or a digest pointer: the element \
                              and everything inside it, in pages."
                    .to_owned(),
                input_schema: read.to_string(),
            },
        ]
    }

    async fn project_of(&self, session: &str) -> Result<String> {
        let dom = self.source.load(session).await?.ok_or_else(|| Error::SessionNotFound(session.to_owned()))?;
        session_project(&dom).ok_or_else(|| Error::Arguments("this session is not in a project".into()))
    }

    async fn run(&self, session: &str, tool: &str, args: &str) -> Result<String> {
        let project = self.project_of(session).await?;
        let args = if args.trim().is_empty() { "{}" } else { args };
        match tool {
            PROJECT_SEARCH => {
                let args: SearchArgs = serde_json::from_str(args).map_err(|e| Error::Arguments(e.to_string()))?;
                let query = args.into_query(session)?;
                let page = search(&*self.source, &*self.access, &self.requester, &project, &query).await?;
                Ok(search_output(&page))
            }
            PROJECT_READ => {
                let args: ReadArgs = serde_json::from_str(args).map_err(|e| Error::Arguments(e.to_string()))?;
                let request = ReadRequest {
                    session: args.session,
                    node: args.node.map(NodeId),
                    offset: args.offset.unwrap_or(0),
                    max_bytes: args.max_bytes.unwrap_or(0),
                };
                let page = read_subtree(&*self.source, &*self.access, &self.requester, &project, &request).await?;
                Ok(read_output(&page))
            }
            other => Err(Error::Arguments(format!("unknown tool `{other}`"))),
        }
    }
}

#[async_trait]
impl HostTools for ProjectTools {
    async fn call(&self, session_id: &str, tool: &str, args: &str) -> Result<String, String> {
        self.run(session_id, tool, args).await.map_err(|e| e.to_string())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: Option<String>,
    mode: Option<String>,
    element: Option<String>,
    #[serde(default)]
    attributes: BTreeMap<String, Option<String>>,
    sessions: Option<Vec<String>>,
    #[serde(default)]
    include_excluded: bool,
    limit: Option<usize>,
    cursor: Option<String>,
}

impl SearchArgs {
    /// The query for a call from `caller`. The caller's own session is left
    /// out unless `sessions` names it.
    fn into_query(self, caller: &str) -> Result<SearchQuery> {
        let mode = match self.mode.as_deref() {
            None | Some("phrase") => TextMode::Phrase,
            Some("all_words") => TextMode::AllWords,
            Some(other) => return Err(Error::Arguments(format!("unknown mode `{other}`"))),
        };
        let kind = match self.element.as_deref() {
            None => None,
            Some(e) => Some(ElementKind::parse(e).ok_or_else(|| Error::Arguments(format!("unknown element `{e}`")))?),
        };
        Ok(SearchQuery {
            text: self.query,
            mode,
            kind,
            attrs: self.attributes.into_iter().map(|(name, value)| AttrFilter { name, value }).collect(),
            skip_session: (!self.sessions.as_ref().is_some_and(|only| only.iter().any(|s| s == caller)))
                .then(|| caller.to_owned()),
            sessions: self.sessions,
            include_excluded: self.include_excluded,
            limit: self.limit.unwrap_or(0),
            cursor: self.cursor,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    session: String,
    node: Option<u64>,
    offset: Option<usize>,
    max_bytes: Option<usize>,
}

fn search_output(page: &SearchPage) -> String {
    let hits: Vec<serde_json::Value> = page
        .hits
        .iter()
        .map(|h| {
            let path: Vec<&str> = h.path.iter().map(|p| p.label.as_str()).collect();
            json!({
                "session": h.session,
                "session_title": h.session_title,
                "node": h.node.0,
                "element": h.kind.as_str(),
                "snippet": h.snippet,
                "path": path.join(" > "),
            })
        })
        .collect();
    json!({"total": page.total, "hits": hits, "next_cursor": page.next_cursor}).to_string()
}

fn read_output(page: &ReadPage) -> String {
    let end = page.offset + page.text.len();
    let mut out = format!(
        "session {} node {} (bytes {}..{} of {})\n",
        page.session, page.node, page.offset, end, page.total_bytes
    );
    out.push_str(&page.text);
    if let Some(next) = page.next_offset {
        out.push_str(&format!("\n[more: call project_read with offset={next}]\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::testing::{Builder, MemorySource, TestAccess, peer};
    use loa_core::{Draft, Value, attr};

    fn tools(access: TestAccess, requester: Requester) -> (ProjectTools, NodeId) {
        let me = Builder::new("me", "proj", 30);
        me.turn("current chat", "", 31);
        let a = Builder::new("a", "proj", 10);
        let t = a.turn("set up net", "done", 11);
        a.call(t, "edit", r#"{"path":"src/net.rs"}"#, "ok");
        let call = a.dom().get(t).unwrap().children[2];
        a.commit(Draft::set(call, attr::TITLE, Value::from("edit src/net.rs")));
        let secret = Builder::new("secret", "proj", 20);
        secret.turn("secret plans for src/net.rs", "", 21);
        let source = MemorySource::with([me.dom(), a.dom(), secret.dom()]);
        (ProjectTools::new(Arc::new(source), Arc::new(access), requester), call)
    }

    #[tokio::test]
    async fn search_then_read_a_hit_through_the_host_tool() {
        let (tools, call) = tools(TestAccess::only(&["me", "a"]), peer());
        let out = tools.call("me", PROJECT_SEARCH, r#"{"query":"src/net.rs","element":"call"}"#).await.unwrap();
        let out: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(out["total"], 1);
        let hit = &out["hits"][0];
        assert_eq!(hit["session"], "a");
        assert_eq!(hit["node"], call.0);
        assert_eq!(hit["element"], "call");
        assert!(hit["path"].as_str().unwrap().starts_with("session a #"));

        let args = json!({"session": "a", "node": call.0}).to_string();
        let text = tools.call("me", PROJECT_READ, &args).await.unwrap();
        assert!(text.contains(&format!("call #{call}")));
        assert!(text.contains(r#"{"path":"src/net.rs"}"#));
    }

    #[tokio::test]
    async fn a_denied_session_is_invisible_to_both_tools() {
        let (tools, _) = tools(TestAccess::only(&["me", "a"]), peer());
        let out = tools.call("me", PROJECT_SEARCH, r#"{"query":"secret"}"#).await.unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&out).unwrap()["total"], 0);
        let denied = tools.call("me", PROJECT_READ, r#"{"session":"secret"}"#).await.unwrap_err();
        let missing = tools.call("me", PROJECT_READ, r#"{"session":"nope"}"#).await.unwrap_err();
        assert_eq!(denied.replace("secret", "X"), missing.replace("nope", "X"));
    }

    #[tokio::test]
    async fn search_leaves_the_calling_chat_out_unless_listed() {
        let (tools, _) = tools(TestAccess::default(), Requester::Local);
        let total = |out: String| serde_json::from_str::<serde_json::Value>(&out).unwrap()["total"].clone();
        assert_eq!(total(tools.call("me", PROJECT_SEARCH, r#"{"query":"current chat"}"#).await.unwrap()), 0);
        let own = r#"{"query":"current chat","sessions":["me"]}"#;
        assert_eq!(total(tools.call("me", PROJECT_SEARCH, own).await.unwrap()), 1);
        assert_eq!(total(tools.call("a", PROJECT_SEARCH, r#"{"query":"current chat"}"#).await.unwrap()), 1);
    }

    #[tokio::test]
    async fn bad_arguments_are_tool_errors() {
        let (tools, _) = tools(TestAccess::default(), Requester::Local);
        assert!(tools.call("me", PROJECT_SEARCH, r#"{"element":"nonsense"}"#).await.is_err());
        assert!(tools.call("me", PROJECT_SEARCH, r#"{"query":"x","bogus":1}"#).await.is_err());
        assert!(tools.call("me", PROJECT_READ, "{}").await.is_err());
        assert!(tools.call("me", "project_write", "{}").await.is_err());
    }
}
