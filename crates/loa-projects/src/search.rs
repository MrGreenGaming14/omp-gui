//! `project_search`: case insensitive text and element search over a project's sessions.
//! Without an element filter a node matches on its own text and string attributes; with
//! one, a node of that kind matches when the text appears anywhere in its subtree.
//! Visible sessions are searched newest first, excluded ones only when asked.

use crate::settings::is_excluded;
use crate::source::{Access, SessionSource, visible_sessions};
use crate::{Error, Result};
use loa_core::{Dom, ElementKind, Node, NodeId, Value, attr};
use loa_util::text::{Paginator, ceil_boundary, clip, floor_boundary};

/// Hits per page.
pub const PAGES: Paginator = Paginator { default: 20, max: 100 };

/// How a text query matches.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TextMode {
    /// The whole query as one substring.
    #[default]
    Phrase,
    /// Every whitespace separated word, anywhere.
    AllWords,
}

/// An attribute the element must have; `value` matches part of it (any list item).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttrFilter {
    pub name: String,
    pub value: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: Option<String>,
    pub mode: TextMode,
    pub kind: Option<ElementKind>,
    pub attrs: Vec<AttrFilter>,
    pub sessions: Option<Vec<String>>,
    /// The calling chat, which already has its own record.
    pub skip_session: Option<String>,
    pub include_excluded: bool,
    /// See [`PAGES`].
    pub limit: usize,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub session: String,
    pub session_title: Option<String>,
    pub node: NodeId,
    pub kind: ElementKind,
    pub snippet: String,
    /// Ancestor labels from the `<session>` down to the hit's parent, joined by ` > `.
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPage {
    pub hits: Vec<SearchHit>,
    /// Hits across every visible session.
    pub total: usize,
    pub next_cursor: Option<String>,
}

/// Bytes of context kept before and after a match.
const BEFORE: usize = 60;
const AFTER: usize = 120;

pub async fn search(source: &dyn SessionSource, access: &dyn Access, project: &str, query: &SearchQuery) -> Result<SearchPage> {
    let matcher = Matcher::new(query)?;
    let skip = match &query.cursor {
        Some(cursor) => cursor.parse::<usize>().map_err(|_| Error::Arguments(format!("bad cursor `{cursor}`")))?,
        None => 0,
    };
    let limit = PAGES.size(query.limit);
    let (mut hits, mut total) = (Vec::new(), 0);
    for info in visible_sessions(source, access, project).await? {
        let wanted = query.sessions.as_ref().is_none_or(|only| only.contains(&info.id));
        if !wanted || query.skip_session.as_ref() == Some(&info.id) {
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
                let mut path: Vec<String> = dom.ancestors(node.id).map(label).collect();
                path.reverse();
                let path = path.join(" > ");
                hits.push(SearchHit { session: info.id.clone(), session_title: info.title.clone(), node: node.id, kind: node.kind, snippet, path });
            }
        }
    }
    let shown = skip + hits.len();
    Ok(SearchPage { hits, total, next_cursor: (shown < total).then(|| shown.to_string()) })
}

struct Matcher<'q> {
    /// Lowercased phrase or words; empty without a text query.
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
            if want.as_ref().is_some_and(|want| !strings(value).any(|v| v.to_lowercase().contains(want.as_str()))) {
                return None;
            }
            prefix += &format!("{name}={} ", render_value(value));
        }
        if self.needles.is_empty() {
            let text = node.text.trim();
            let shown = if text.is_empty() { node.str_attr(attr::TITLE).unwrap_or_default() } else { text };
            return Some(format!("{prefix}{}", one_line(clip(shown, BEFORE + AFTER))).trim_end().to_owned());
        }
        // An element filter widens the text scope to the subtree.
        let scope: Vec<&Node> = if self.kind.is_some() { dom.descendants(node.id).collect() } else { vec![node] };
        let mut found = vec![false; self.needles.len()];
        let mut snippet = None;
        for field in scope.iter().flat_map(|n| std::iter::once(n.text.as_str()).chain(n.attrs.values().flat_map(strings))) {
            let folded = Folded::new(field);
            for (i, needle) in self.needles.iter().enumerate() {
                if let Some(at) = folded.lower.find(needle.as_str()) {
                    found[i] = true;
                    snippet.get_or_insert_with(|| window(field, folded.origin(at), folded.origin(at + needle.len())));
                }
            }
            if found.iter().all(|f| *f) {
                return Some(format!("{prefix}{}", snippet.unwrap_or_default()));
            }
        }
        None
    }
}

/// Lowercased text that maps each byte back to its offset in the original.
struct Folded {
    lower: String,
    origin: Vec<usize>,
}

impl Folded {
    fn new(text: &str) -> Self {
        let (mut lower, mut origin) = (String::with_capacity(text.len()), Vec::with_capacity(text.len() + 1));
        for (at, ch) in text.char_indices() {
            ch.to_lowercase().for_each(|low| lower.push(low));
            origin.resize(lower.len(), at);
        }
        origin.push(text.len());
        Self { lower, origin }
    }

    fn origin(&self, at: usize) -> usize {
        self.origin[at.min(self.origin.len() - 1)]
    }
}

fn strings(value: &Value) -> Box<dyn Iterator<Item = &str> + '_> {
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

/// `text[start..end]` with context around it, on one line.
fn window(text: &str, start: usize, end: usize) -> String {
    let from = floor_boundary(text, start.saturating_sub(BEFORE));
    let to = ceil_boundary(text, end.saturating_add(AFTER));
    let (pre, post) = (if from > 0 { "…" } else { "" }, if to < text.len() { "…" } else { "" });
    format!("{pre}{}{post}", one_line(&text[from..to]))
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A short label for a node in a hit path.
fn label(node: &Node) -> String {
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
