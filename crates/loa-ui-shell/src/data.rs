//! Plain data the app hands to the shell: session summaries and node status.

use loa_core::schema::{DecisionStatus, ElementKind, Role, attr};
use loa_core::Dom;

/// Longest title derived from a session's first prompt, in characters.
const DERIVED_TITLE_CHARS: usize = 60;

/// Title shown for a session that has neither a title nor a prompt yet.
pub const UNTITLED: &str = "New session";

/// One session as the sidebar and top bar show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// App wide session id (`<session id>`).
    pub id: String,
    /// Display title.
    pub title: String,
    /// Project the session belongs to, if any.
    pub project: Option<String>,
    /// Public key of the node that runs the session (`<session node>`).
    pub node: String,
    /// Whether the user pinned the session. App data, not session data.
    pub pinned: bool,
    /// Whether a decision in the session waits for an answer.
    pub waiting: bool,
    /// Unix milliseconds of the last known change.
    pub updated: i64,
}

impl SessionSummary {
    /// Reads the summary of a session from its DOM.
    ///
    /// The title is the `title` attribute, else the first line of the first
    /// user prompt, else [`UNTITLED`]. `updated` is the latest of `created`,
    /// `started` and `ended` anywhere in the tree. `waiting` is set when any
    /// `<decision>` has `status="waiting"`. Returns `None` when the DOM has no
    /// `<session>` root or the root lacks `id` or `node`.
    pub fn from_dom(dom: &Dom, pinned: bool) -> Option<Self> {
        let root = dom.root().filter(|node| node.kind == ElementKind::Session)?;
        let id = root.str_attr(attr::ID)?.to_owned();
        let node = root.str_attr(attr::NODE)?.to_owned();

        let mut updated = root.int_attr(attr::CREATED).unwrap_or(0);
        let mut waiting = false;
        let mut first_prompt: Option<&str> = None;
        for element in dom.iter() {
            for name in [attr::STARTED, attr::ENDED] {
                if let Some(time) = element.int_attr(name) {
                    updated = updated.max(time);
                }
            }
            match element.kind {
                ElementKind::Decision => {
                    waiting |= element.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str());
                }
                ElementKind::Message
                    if first_prompt.is_none() && element.str_attr(attr::ROLE) == Some(Role::User.as_str()) =>
                {
                    first_prompt = Some(element.text.as_str());
                }
                _ => {}
            }
        }

        let title = match root.str_attr(attr::TITLE) {
            Some(title) if !title.trim().is_empty() => title.trim().to_owned(),
            _ => first_prompt.and_then(derived_title).unwrap_or_else(|| UNTITLED.to_owned()),
        };

        Some(Self {
            id,
            title,
            project: root.str_attr(attr::PROJECT).map(str::to_owned),
            node,
            pinned,
            waiting,
            updated,
        })
    }
}

/// First non blank line of a prompt, cut to [`DERIVED_TITLE_CHARS`].
fn derived_title(prompt: &str) -> Option<String> {
    let line = prompt.lines().map(str::trim).find(|line| !line.is_empty())?;
    match line.char_indices().nth(DERIVED_TITLE_CHARS) {
        Some((cut, _)) => Some(format!("{}…", line[..cut].trim_end())),
        None => Some(line.to_owned()),
    }
}

/// A node of the p2p network as the shell shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStatus {
    /// Public key, as written in `<session node>`.
    pub key: String,
    /// Human name, e.g. the host name or the name the user gave the peer.
    pub name: String,
    /// Whether the node is reachable right now.
    pub online: bool,
    /// Whether this is the node the app runs on.
    pub local: bool,
}

/// The known nodes, looked up by key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Nodes {
    list: Vec<NodeStatus>,
}

impl Nodes {
    /// Builds the directory from the app's node list.
    pub fn new(list: Vec<NodeStatus>) -> Self {
        Self { list }
    }

    /// The node with this key.
    pub fn get(&self, key: &str) -> Option<&NodeStatus> {
        self.list.iter().find(|node| node.key == key)
    }

    /// Display name of the node with this key: its name, or a short form of
    /// the key when the node is unknown.
    pub fn name_of<'a>(&'a self, key: &'a str) -> &'a str {
        self.get(key).map_or_else(|| short_key(key), |node| node.name.as_str())
    }

    /// Whether the node with this key is reachable. Unknown nodes are offline.
    pub fn is_online(&self, key: &str) -> bool {
        self.get(key).is_some_and(|node| node.online)
    }

    /// Every known node.
    pub fn iter(&self) -> impl Iterator<Item = &NodeStatus> {
        self.list.iter()
    }
}

/// First eight characters of a node key, used when the node has no name.
pub fn short_key(key: &str) -> &str {
    key.char_indices().nth(8).map_or(key, |(cut, _)| &key[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, NodeId, Value};

    fn session(dom: &mut Dom, attrs: Vec<(&str, Value)>) -> NodeId {
        let patch = dom
            .commit(Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""), 1)
            .expect("insert session");
        patch.node
    }

    fn insert(dom: &mut Dom, parent: NodeId, kind: ElementKind, attrs: Vec<(&str, Value)>, text: &str) -> NodeId {
        let patch = dom.commit(Draft::insert(parent, kind, attrs, text), 1).expect("insert");
        patch.node
    }

    fn base_attrs() -> Vec<(&'static str, Value)> {
        vec![
            (attr::ID, Value::from("s1")),
            (attr::HARNESS, Value::from("omp")),
            (attr::NODE, Value::from("key-laptop")),
            (attr::CREATED, Value::Int(100)),
        ]
    }

    #[test]
    fn untitled_session_takes_first_prompt_line_and_waiting_decision() {
        let mut dom = Dom::new();
        let root = session(&mut dom, base_attrs());
        let turn = insert(
            &mut dom,
            root,
            ElementKind::Turn,
            vec![(attr::STATUS, Value::from("waiting")), (attr::STARTED, Value::Int(500))],
            "",
        );
        insert(
            &mut dom,
            turn,
            ElementKind::Message,
            vec![(attr::ROLE, Value::from("user")), (attr::STATUS, Value::from("done"))],
            "\n  Fix the flaky deploy test  \nmore detail",
        );
        let call = insert(
            &mut dom,
            turn,
            ElementKind::Call,
            vec![
                (attr::TOOL, Value::from("bash")),
                (attr::CALL_ID, Value::from("c1")),
                (attr::STATUS, Value::from("running")),
                (attr::STARTED, Value::Int(900)),
            ],
            "",
        );
        insert(
            &mut dom,
            call,
            ElementKind::Decision,
            vec![(attr::KIND, Value::from("approval")), (attr::STATUS, Value::from("waiting"))],
            "",
        );

        let summary = SessionSummary::from_dom(&dom, true).expect("summary");
        assert_eq!(summary.title, "Fix the flaky deploy test");
        assert!(summary.waiting);
        assert!(summary.pinned);
        assert_eq!(summary.updated, 900);
        assert_eq!(summary.node, "key-laptop");
    }

    #[test]
    fn answered_decision_is_not_waiting_and_title_attribute_wins() {
        let mut dom = Dom::new();
        let mut attrs = base_attrs();
        attrs.push((attr::TITLE, Value::from("Release notes")));
        attrs.push((attr::PROJECT, Value::from("loa")));
        let root = session(&mut dom, attrs);
        let turn = insert(&mut dom, root, ElementKind::Turn, vec![(attr::STATUS, Value::from("running"))], "");
        insert(
            &mut dom,
            turn,
            ElementKind::Message,
            vec![(attr::ROLE, Value::from("user")), (attr::STATUS, Value::from("done"))],
            "write the notes",
        );
        insert(
            &mut dom,
            turn,
            ElementKind::Decision,
            vec![(attr::KIND, Value::from("question")), (attr::STATUS, Value::from("answered"))],
            "",
        );

        let summary = SessionSummary::from_dom(&dom, false).expect("summary");
        assert_eq!(summary.title, "Release notes");
        assert_eq!(summary.project.as_deref(), Some("loa"));
        assert!(!summary.waiting);
        assert_eq!(summary.updated, 100);
    }

    #[test]
    fn long_prompt_title_is_cut_on_a_char_boundary() {
        let prompt = "é".repeat(80);
        let title = derived_title(&prompt).expect("title");
        assert_eq!(title.chars().count(), DERIVED_TITLE_CHARS + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn unknown_node_shows_short_key() {
        let nodes = Nodes::new(vec![NodeStatus {
            key: "abc".into(),
            name: "desk".into(),
            online: true,
            local: true,
        }]);
        assert_eq!(nodes.name_of("abc"), "desk");
        assert_eq!(nodes.name_of("0123456789abcdef"), "01234567");
        assert!(!nodes.is_online("0123456789abcdef"));
    }
}
