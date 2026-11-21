//! Plain data the app hands to the shell: session summaries, node status and usage.

use loa_core::Dom;
use loa_core::schema::{DecisionStatus, ElementKind, Role, attr};

/// Longest title derived from a session's first prompt, in characters.
const DERIVED_TITLE_CHARS: usize = 60;

/// Title of a session with neither a title nor a prompt yet.
pub const UNTITLED: &str = "New session";

/// One session as the sidebar and top bar show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub project: Option<String>,
    /// Public key of the node that runs the session.
    pub node: String,
    /// App data, not session data.
    pub pinned: bool,
    /// A decision in the session waits for an answer.
    pub waiting: bool,
    /// Unix milliseconds of the last known change.
    pub updated: i64,
}

impl SessionSummary {
    /// Reads a summary from a session DOM: the `title` attribute, else the first line of the
    /// first user prompt; `updated` is the latest `created`/`started`/`ended`; `waiting` when a
    /// decision waits. `None` without a `<session>` root carrying `id` and `node`.
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
            _ => prompt.and_then(derived_title).unwrap_or_else(|| UNTITLED.to_owned()),
        };
        let (id, node) = (root.str_attr(attr::ID)?.to_owned(), root.str_attr(attr::NODE)?.to_owned());
        Some(Self { id, title, project: root.str_attr(attr::PROJECT).map(str::to_owned), node, pinned, waiting, updated })
    }
}

/// First non blank line of a prompt, cut to [`DERIVED_TITLE_CHARS`].
fn derived_title(prompt: &str) -> Option<String> {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(match line.char_indices().nth(DERIVED_TITLE_CHARS) {
        Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
        None => line.to_owned(),
    })
}

/// The sidebar footer's usage meter: the tightest limit across providers.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageMeter {
    /// Used share, 0 to 1.
    pub fraction: f32,
    /// What it measures, e.g. "7d".
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

/// A node of the p2p network as the shell shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStatus {
    /// Public key, as written in `<session node>`.
    pub key: String,
    pub name: String,
    pub online: bool,
    /// This is the node the app runs on.
    pub local: bool,
}

/// The known nodes, looked up by key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nodes(Vec<NodeStatus>);

impl Nodes {
    pub fn new(list: Vec<NodeStatus>) -> Self { Self(list) }
    pub fn get(&self, key: &str) -> Option<&NodeStatus> { self.0.iter().find(|n| n.key == key) }
    /// The node's name, or a short form of an unknown key.
    pub fn name_of<'a>(&'a self, key: &'a str) -> &'a str { self.get(key).map_or_else(|| short_key(key), |n| n.name.as_str()) }
    pub fn is_online(&self, key: &str) -> bool { self.get(key).is_some_and(|n| n.online) }
    pub fn iter(&self) -> impl Iterator<Item = &NodeStatus> { self.0.iter() }
}

/// First eight characters of a node key.
pub fn short_key(key: &str) -> &str { key.char_indices().nth(8).map_or(key, |(cut, _)| &key[..cut]) }

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, NodeId, Value};

    #[test]
    fn summary_reads_title_waiting_and_latest_time() {
        let mut dom = Dom::new();
        let mut add = |parent, kind, attrs: Vec<(&str, Value)>, text: &str| dom.commit(Draft::insert(parent, kind, attrs, text), 1).unwrap().node;
        let root = add(NodeId::DOCUMENT, ElementKind::Session, vec![(attr::ID, "s1".into()), (attr::HARNESS, "omp".into()), (attr::NODE, "key".into()), (attr::CREATED, Value::Int(100))], "");
        let turn = add(root, ElementKind::Turn, vec![(attr::STATUS, "waiting".into()), (attr::STARTED, Value::Int(500))], "");
        add(turn, ElementKind::Message, vec![(attr::ROLE, "user".into()), (attr::STATUS, "done".into())], "\n  Fix the flaky deploy test  \nmore");
        let call = add(turn, ElementKind::Call, vec![(attr::TOOL, "bash".into()), (attr::CALL_ID, "c1".into()), (attr::STATUS, "running".into()), (attr::STARTED, Value::Int(900))], "");
        add(call, ElementKind::Decision, vec![(attr::KIND, "approval".into()), (attr::STATUS, "waiting".into())], "");
        let s = SessionSummary::from_dom(&dom, true).unwrap();
        assert_eq!((s.title.as_str(), s.waiting, s.pinned, s.updated), ("Fix the flaky deploy test", true, true, 900));
        let title = derived_title(&"é".repeat(80)).unwrap();
        assert!(title.chars().count() == DERIVED_TITLE_CHARS + 1 && title.ends_with('…'));
        let nodes = Nodes::new(vec![NodeStatus { key: "abc".into(), name: "desk".into(), online: true, local: true }]);
        assert_eq!((nodes.name_of("abc"), nodes.name_of("0123456789abcdef"), nodes.is_online("x")), ("desk", "01234567", false));
    }
}
