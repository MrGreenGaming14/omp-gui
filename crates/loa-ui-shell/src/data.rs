//! Plain data the app hands to the shell: session summaries, node status and usage.

use loa_core::Dom;
use loa_core::schema::{DecisionStatus, ElementKind, Role, attr};

const DERIVED_TITLE_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub project: Option<String>,
    /// Public key of the node that runs the session.
    pub node: String,
    pub pinned: bool,
    /// A decision in the session waits for an answer.
    pub waiting: bool,
    /// Unix milliseconds of the last known change.
    pub updated: i64,
}

impl SessionSummary {
    /// Reads a summary from a session DOM: the `title` attribute, else the first line of the
    /// first user prompt; `updated` is the latest `created`/`started`/`ended`. `None` without
    /// a `<session>` root carrying `id` and `node`.
    pub fn from_dom(dom: &Dom, pinned: bool) -> Option<Self> {
        let root = dom.root().filter(|n| n.kind == ElementKind::Session)?;
        let (mut updated, mut waiting, mut prompt) = (root.int_attr(attr::CREATED).unwrap_or(0), false, None);
        for e in dom.iter() {
            updated = [attr::STARTED, attr::ENDED].iter().filter_map(|a| e.int_attr(a)).fold(updated, i64::max);
            match e.kind {
                ElementKind::Decision => waiting |= e.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str()),
                ElementKind::Message if prompt.is_none() && e.str_attr(attr::ROLE) == Some(Role::User.as_str()) => prompt = Some(e.text.as_str()),
                _ => {}
            }
        }
        let title = match root.str_attr(attr::TITLE).map(str::trim) {
            Some(t) if !t.is_empty() => t.to_owned(),
            _ => prompt.and_then(derived_title).unwrap_or_else(|| "New session".to_owned()),
        };
        let (id, node) = (root.str_attr(attr::ID)?.to_owned(), root.str_attr(attr::NODE)?.to_owned());
        Some(Self { id, title, project: root.str_attr(attr::PROJECT).map(str::to_owned), node, pinned, waiting, updated })
    }
}

fn derived_title(prompt: &str) -> Option<String> {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(match line.char_indices().nth(DERIVED_TITLE_CHARS) {
        Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
        None => line.to_owned(),
    })
}

/// The sidebar footer's usage meter: the used share (0 to 1) of the tightest limit and what it
/// measures, e.g. "7d".
#[derive(Debug, Clone, PartialEq)]
pub struct UsageMeter {
    pub fraction: f32,
    pub label: String,
}

/// Which machine's sessions the sidebar lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeFilter {
    All,
    Node(String),
}

impl NodeFilter {
    pub fn matches(&self, node: &str) -> bool { if let Self::Node(key) = self { key == node } else { true } }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStatus {
    /// Public key, as written in `<session node>`.
    pub key: String,
    pub name: String,
    pub online: bool,
    /// This is the node the app runs on.
    pub local: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nodes(Vec<NodeStatus>);

impl Nodes {
    pub fn new(list: Vec<NodeStatus>) -> Self { Self(list) }
    pub fn get(&self, key: &str) -> Option<&NodeStatus> { self.0.iter().find(|n| n.key == key) }
    /// The node's name, or the first eight characters of an unknown key.
    pub fn name_of<'a>(&'a self, key: &'a str) -> &'a str { self.get(key).map_or_else(|| key.char_indices().nth(8).map_or(key, |(cut, _)| &key[..cut]), |n| n.name.as_str()) }
    pub fn is_online(&self, key: &str) -> bool { self.get(key).is_some_and(|n| n.online) }
    pub fn iter(&self) -> impl Iterator<Item = &NodeStatus> { self.0.iter() }
}
