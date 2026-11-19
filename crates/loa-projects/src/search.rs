//! `project_search`: text and element search over a project's session DOMs.
//!
//! Text matching is case insensitive, either as one phrase or as all words.
//! Without an element filter every node is matched on its own text and
//! string attributes. With one, a node of that kind matches when the text
//! appears anywhere in its subtree, so `element = call` with a file path finds
//! every call whose input or result mentions it.
//!
//! Only sessions the requester may open are searched, newest first, nodes in
//! document order. Excluded sessions are skipped unless asked for.

use crate::settings::is_excluded;
use crate::source::{Access, Requester, SessionSource, visible_sessions};
use crate::text::{Folded, ceil_boundary, clip, floor_boundary};
use crate::{Error, Result};
use loa_core::{Dom, ElementKind, Node, NodeId, Value, attr};

/// Hits per page when the query doesn't say.
pub const DEFAULT_LIMIT: usize = 20;
/// Most hits per page.
pub const MAX_LIMIT: usize = 100;

/// How a text query matches.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum TextMode {
    /// The whole query as one substring.
    #[default]
    Phrase,
    /// Every whitespace separated word, anywhere.
    AllWords,
}

/// An attribute the matched element must have.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrFilter {
    pub name: String,
    /// Case insensitive substring of the value (any item of a list). `None`
    /// only requires the attribute.
    pub value: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: Option<String>,
    pub mode: TextMode,
    pub kind: Option<ElementKind>,
    pub attrs: Vec<AttrFilter>,
    /// Restrict to these sessions.
    pub sessions: Option<Vec<String>>,
    /// Leave this session out: the chat that searches, which already has its
    /// own record in context.
    pub skip_session: Option<String>,
    pub include_excluded: bool,
    /// Hits per page, clamped to 1..=[`MAX_LIMIT`]; 0 means [`DEFAULT_LIMIT`].
    pub limit: usize,
    /// From the previous page's `next_cursor`.
    pub cursor: Option<String>,
}

/// One ancestor of a hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathStep {
    pub node: NodeId,
    pub kind: ElementKind,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub session: String,
    pub session_title: Option<String>,
    pub node: NodeId,
    pub kind: ElementKind,
    pub snippet: String,
    /// Ancestors from the `<session>` down to the hit's parent.
    pub path: Vec<PathStep>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    /// Hits across every visible session.
    pub total: usize,
    pub next_cursor: Option<String>,
}

/// Bytes of context kept before and after a match in a snippet.
const BEFORE: usize = 60;
const AFTER: usize = 120;

/// Runs `query` over the sessions of `project` that `requester` may open.
pub async fn search(
    source: &dyn SessionSource,
    access: &dyn Access,
    requester: &Requester,
    project: &str,
    query: &SearchQuery,
) -> Result<SearchPage> {
    let matcher = Matcher::new(query)?;
    let skip = match &query.cursor {
        Some(cursor) => cursor.parse::<usize>().map_err(|_| Error::Arguments(format!("bad cursor `{cursor}`")))?,
        None => 0,
    };
    let limit = match query.limit {
        0 => DEFAULT_LIMIT,
        n => n.min(MAX_LIMIT),
    };
    let mut hits = Vec::with_capacity(limit);
    let mut total = 0;
    for info in visible_sessions(source, access, requester, project).await? {
        if query.sessions.as_ref().is_some_and(|only| !only.contains(&info.id))
            || query.skip_session.as_ref() == Some(&info.id)
        {
            continue;
        }
        let Some(dom) = source.load(&info.id).await? else { continue };
        if !query.include_excluded && is_excluded(&dom) {
            continue;
        }
        for node in dom.iter() {
            let Some(snippet) = matcher.matches(&dom, node) else { continue };
            total += 1;
            if total > skip && hits.len() < limit {
                hits.push(SearchHit {
                    session: info.id.clone(),
                    session_title: info.title.clone(),
                    node: node.id,
                    kind: node.kind,
                    snippet,
                    path: path_of(&dom, node.id),
                });
            }
        }
    }
    let shown = skip + hits.len();
    Ok(SearchPage { hits, total, next_cursor: (shown < total).then(|| shown.to_string()) })
}

struct Matcher<'q> {
    /// Lowercased phrase or words; empty when there is no text query.
    needles: Vec<String>,
    kind: Option<ElementKind>,
    attrs: Vec<(&'q str, Option<String>)>,
}

impl<'q> Matcher<'q> {
    fn new(query: &'q SearchQuery) -> Result<Self> {
        let text = query.text.as_deref().map(str::trim).unwrap_or_default();
        let needles = match query.mode {
            _ if text.is_empty() => Vec::new(),
            TextMode::Phrase => vec![text.to_lowercase()],
            TextMode::AllWords => text.split_whitespace().map(str::to_lowercase).collect(),
        };
        if needles.is_empty() && query.kind.is_none() && query.attrs.is_empty() {
            return Err(Error::Arguments("give a text query, an element type or an attribute".into()));
        }
        let attrs = query.attrs.iter().map(|a| (a.name.as_str(), a.value.as_deref().map(str::to_lowercase))).collect();
        Ok(Self { needles, kind: query.kind, attrs })
    }

    /// The snippet when `node` matches.
    fn matches(&self, dom: &Dom, node: &Node) -> Option<String> {
        if self.kind.is_some_and(|k| k != node.kind) {
            return None;
        }
        let mut prefix = String::new();
        for (name, want) in &self.attrs {
            let value = node.attr(name)?;
            if let Some(want) = want
                && !value_strings(value).any(|v| v.to_lowercase().contains(want.as_str()))
            {
                return None;
            }
            prefix.push_str(&format!("{name}={} ", render_value(value)));
        }
        if self.needles.is_empty() {
            let text = node.text.trim();
            let shown = if text.is_empty() { node.str_attr(attr::TITLE).unwrap_or_default() } else { text };
            return Some(format!("{prefix}{}", one_line(clip(shown, BEFORE + AFTER))).trim_end().to_owned());
        }
        // An element filter widens the text scope to the subtree.
        let scope: Vec<&Node> = if self.kind.is_some() { dom.descendants(node.id).collect() } else { vec![node] };
        let mut found: Vec<bool> = vec![false; self.needles.len()];
        let mut snippet = None;
        for field in scope.iter().flat_map(|n| fields(n)) {
            let folded = Folded::new(field);
            for (i, needle) in self.needles.iter().enumerate() {
                if let Some(at) = folded.lower.find(needle.as_str()) {
                    found[i] = true;
                    if snippet.is_none() {
                        let start = folded.origin(at);
                        let end = folded.origin(at + needle.len());
                        snippet = Some(window(field, start, end));
                    }
                }
            }
            if found.iter().all(|f| *f) {
                return Some(format!("{prefix}{}", snippet.unwrap_or_default()));
            }
        }
        None
    }
}

/// A node's searchable strings: its text, then its string attributes.
fn fields(node: &Node) -> impl Iterator<Item = &str> {
    std::iter::once(node.text.as_str()).chain(node.attrs.values().flat_map(value_strings))
}

fn value_strings(value: &Value) -> Box<dyn Iterator<Item = &str> + '_> {
    match value {
        Value::Str(s) => Box::new(std::iter::once(s.as_str())),
        Value::List(_) => Box::new(value.str_items()),
        _ => Box::new(std::iter::empty()),
    }
}

pub(crate) fn render_value(value: &Value) -> String {
    match value {
        Value::Bool(v) => v.to_string(),
        Value::Int(v) => v.to_string(),
        Value::Float(v) => v.to_string(),
        Value::Str(v) if v.is_empty() || v.contains(char::is_whitespace) => format!("{v:?}"),
        Value::Str(v) => v.clone(),
        Value::Bytes(v) => format!("<{} bytes>", v.len()),
        Value::List(items) => format!("[{}]", items.iter().map(render_value).collect::<Vec<_>>().join(", ")),
    }
}

/// `text[start..end]` with some context around it, on one line.
fn window(text: &str, start: usize, end: usize) -> String {
    let from = floor_boundary(text, start.saturating_sub(BEFORE));
    let to = ceil_boundary(text, end.saturating_add(AFTER));
    let mut out = String::new();
    if from > 0 {
        out.push('…');
    }
    out.push_str(&one_line(&text[from..to]));
    if to < text.len() {
        out.push('…');
    }
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A short label for a node in a hit path.
pub(crate) fn label(node: &Node) -> String {
    let detail = match node.kind {
        ElementKind::Session => node.str_attr(attr::TITLE).or(node.str_attr(attr::ID)),
        ElementKind::Message => node.str_attr(attr::ROLE),
        ElementKind::Call => node.str_attr(attr::TOOL),
        ElementKind::Decision | ElementKind::Job => node.str_attr(attr::KIND),
        _ => None,
    };
    match detail {
        Some(d) => format!("{} {} #{}", node.kind, one_line(clip(d, 60)), node.id),
        None => format!("{} #{}", node.kind, node.id),
    }
}

fn path_of(dom: &Dom, id: NodeId) -> Vec<PathStep> {
    let mut path: Vec<PathStep> =
        dom.ancestors(id).map(|n| PathStep { node: n.id, kind: n.kind, label: label(n) }).collect();
    path.reverse();
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::exclude_draft;
    use crate::source::testing::{Builder, MemorySource, TestAccess, peer};
    use loa_core::{Draft, ElementKind as K};

    fn project() -> MemorySource {
        let a = Builder::new("a", "proj", 10);
        let t = a.turn("Make the Listener bind 31415", "Bound the listener.", 11);
        a.call(t, "edit", r#"{"path":"src/net.rs"}"#, "ok");
        a.commit(Draft::insert(
            t,
            K::Decision,
            [(attr::KIND, Value::from("approval")), (attr::ANSWER, Value::from(vec!["allow_once".to_owned()]))],
            "edit src/net.rs",
        ));
        let b = Builder::new("b", "proj", 20);
        let t = b.turn("Read the net code", "The listener is in net.rs", 21);
        b.call(t, "read", r#"{"path":"src/net.rs"}"#, "fn listen()");
        b.commit(Draft::insert(
            t,
            K::Decision,
            [(attr::KIND, Value::from("question")), (attr::ANSWER, Value::from(vec!["TCP fallback".to_owned()]))],
            "Which transport?",
        ));
        let other = Builder::new("o", "elsewhere", 30);
        other.turn("listener in another project", "", 31);
        MemorySource::with([a.dom(), b.dom(), other.dom()])
    }

    fn q(text: &str) -> SearchQuery {
        SearchQuery { text: Some(text.into()), ..Default::default() }
    }

    async fn run(source: &MemorySource, access: &TestAccess, requester: &Requester, query: &SearchQuery) -> SearchPage {
        search(source, access, requester, "proj", query).await.unwrap()
    }

    #[tokio::test]
    async fn phrase_search_is_case_insensitive_with_snippet_and_path() {
        let source = project();
        let page = run(&source, &TestAccess::default(), &Requester::Local, &q("the LISTENER")).await;
        let hit = &page.hits[0];
        assert_eq!((hit.session.as_str(), hit.kind), ("b", K::Message));
        assert!(hit.snippet.contains("The listener is in net.rs"));
        let path: Vec<ElementKind> = hit.path.iter().map(|p| p.kind).collect();
        assert_eq!(path, [K::Session, K::Turn]);
        // Both messages of session a match too; the other project never does.
        assert_eq!(page.total, 3);
        assert!(page.hits.iter().all(|h| h.session != "o"));
    }

    #[tokio::test]
    async fn all_words_mode_needs_every_word() {
        let source = project();
        let mut query = q("bind listener 31415");
        assert_eq!(run(&source, &TestAccess::default(), &Requester::Local, &query).await.total, 0);
        query.mode = TextMode::AllWords;
        let page = run(&source, &TestAccess::default(), &Requester::Local, &query).await;
        assert_eq!(page.total, 1);
        assert_eq!(page.hits[0].session, "a");
    }

    #[tokio::test]
    async fn every_decision_answer_and_every_call_touching_a_file() {
        let source = project();
        let decisions = SearchQuery {
            kind: Some(K::Decision),
            attrs: vec![AttrFilter { name: attr::ANSWER.into(), value: None }],
            ..Default::default()
        };
        let page = run(&source, &TestAccess::default(), &Requester::Local, &decisions).await;
        assert_eq!(page.total, 2);
        assert!(page.hits.iter().any(|h| h.snippet.starts_with("answer=[\"TCP fallback\"]")));

        let calls = SearchQuery { kind: Some(K::Call), ..q("src/net.rs") };
        let page = run(&source, &TestAccess::default(), &Requester::Local, &calls).await;
        let tools: Vec<(&str, K)> = page.hits.iter().map(|h| (h.session.as_str(), h.kind)).collect();
        assert_eq!(tools, [("b", K::Call), ("a", K::Call)]);
        assert!(page.hits[0].snippet.contains("src/net.rs"));
    }

    #[tokio::test]
    async fn cursor_pages_through_all_hits() {
        let source = project();
        let query = SearchQuery { kind: Some(K::Message), limit: 3, ..Default::default() };
        let first = run(&source, &TestAccess::default(), &Requester::Local, &query).await;
        assert_eq!((first.hits.len(), first.total), (3, 4));
        let next = SearchQuery { cursor: first.next_cursor.clone(), ..query };
        let second = run(&source, &TestAccess::default(), &Requester::Local, &next).await;
        assert_eq!(second.hits.len(), 1);
        assert_eq!(second.next_cursor, None);
        assert!(second.hits.iter().all(|h| !first.hits.contains(h)));
    }

    #[tokio::test]
    async fn denied_sessions_leave_no_trace_in_hits_or_counts() {
        let source = project();
        let peer = peer();
        let access = TestAccess::only(&["b"]);
        let page = run(&source, &access, &peer, &q("listener")).await;
        assert!(page.hits.iter().all(|h| h.session == "b"));
        // Same result as a project where session a never existed.
        let alone = MemorySource::default();
        alone.put(source.doms.lock().unwrap()["b"].as_ref().clone());
        assert_eq!(page, run(&alone, &access, &peer, &q("listener")).await);
        let scoped = SearchQuery { sessions: Some(vec!["a".into()]), ..q("listener") };
        assert_eq!(run(&source, &access, &peer, &scoped).await.total, 0);
    }

    #[tokio::test]
    async fn excluded_sessions_are_skipped_unless_asked_for() {
        let source = project();
        let a = source.doms.lock().unwrap()["a"].as_ref().clone();
        let sink = loa_core::MemorySink::from_dom(a.clone());
        loa_core::PatchSink::commit(&sink, exclude_draft(&a, true).unwrap()).unwrap();
        source.put(sink.dom());
        assert_eq!(run(&source, &TestAccess::default(), &Requester::Local, &q("listener")).await.total, 1);
        let all = SearchQuery { include_excluded: true, ..q("listener") };
        assert_eq!(run(&source, &TestAccess::default(), &Requester::Local, &all).await.total, 3);
    }

    #[tokio::test]
    async fn an_empty_query_is_rejected() {
        let err = search(&project(), &TestAccess::default(), &Requester::Local, "proj", &SearchQuery::default()).await;
        assert!(matches!(err, Err(Error::Arguments(_))));
    }
}
