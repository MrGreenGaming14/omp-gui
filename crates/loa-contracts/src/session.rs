//! What a session is at a glance: the summary lists and title bars show.

use loa_core::schema::{DecisionStatus, ElementKind, Role, TEXT, attr};
use loa_core::{Dom, Node, Op, Patch};

/// Root attributes the summary copies or derives everything else from.
const ROOT_ATTRS: [&str; 5] = [attr::TITLE, attr::PROJECT, attr::ID, attr::NODE, attr::CREATED];

const DERIVED_TITLE_CHARS: usize = 60;

/// Title of a session that has neither a title nor a prompt yet.
const UNTITLED: &str = "New session";

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
    /// Unix ms of creation or of the latest `ended`. A turn that is still running does not move
    /// the session.
    pub updated: i64,
}

impl SessionSummary {
    /// Reads a `<session id node>` DOM: title attr or first user prompt line; `updated` is the
    /// latest of `created` and every `ended`.
    pub fn from_dom(dom: &Dom, pinned: bool) -> Option<Self> {
        let root = dom.root().filter(|n| n.kind == ElementKind::Session)?;
        let (mut updated, mut waiting) = (root.int_attr(attr::CREATED).unwrap_or(0), false);
        for e in dom.iter() {
            updated = updated.max(ended_at(e));
            waiting |= is_waiting(e);
        }
        let (id, node) = (root.str_attr(attr::ID)?.to_owned(), root.str_attr(attr::NODE)?.to_owned());
        Some(Self { id, title: title_of(dom, root), project: root.str_attr(attr::PROJECT).map(str::to_owned), node, pinned, waiting, updated })
    }

    /// Brings the summary in line with `patch`, already applied to `dom`, and says whether it
    /// changed. Walks the tree only for the rare patches that can lower `updated` or clear
    /// `waiting`; streamed appends to replies cost a lookup. The result always equals
    /// [`Self::from_dom`].
    pub fn apply(&mut self, dom: &Dom, patch: &Patch) -> bool {
        let on_root = dom.root().is_some_and(|root| root.id == patch.node);
        match (&patch.op, dom.get(patch.node)) {
            (Op::Remove | Op::Insert { kind: ElementKind::Session, .. }, _) => self.reload(dom),
            (Op::Set { name, .. }, _) if on_root && ROOT_ATTRS.contains(&name.as_str()) => self.reload(dom),
            (_, None) => false,
            (Op::Insert { .. }, Some(node)) => {
                let mut changed = false;
                let time = ended_at(node);
                if time > self.updated {
                    self.updated = time;
                    changed = true;
                }
                if is_waiting(node) && !self.waiting {
                    self.waiting = true;
                    changed = true;
                }
                if is_user_message(node) {
                    changed |= self.retitle(dom);
                }
                changed
            }
            (Op::Set { name, .. }, Some(node)) => match name.as_str() {
                attr::ENDED => {
                    let updated = match node.int_attr(name) {
                        Some(time) if time >= self.updated => time,
                        // The value fell or went away: the latest may be elsewhere.
                        _ => dom.iter().map(ended_at).fold(dom.root().and_then(|root| root.int_attr(attr::CREATED)).unwrap_or(0), i64::max),
                    };
                    std::mem::replace(&mut self.updated, updated) != updated
                }
                attr::STATUS if node.kind == ElementKind::Decision => {
                    let waiting = dom.iter().any(is_waiting);
                    std::mem::replace(&mut self.waiting, waiting) != waiting
                }
                attr::ROLE | TEXT if node.kind == ElementKind::Message => self.retitle(dom),
                _ => false,
            },
            (Op::Append { .. }, Some(node)) => is_user_message(node) && self.retitle(dom),
        }
    }

    /// Reads the whole summary again; keeps it when the DOM no longer has one.
    fn reload(&mut self, dom: &Dom) -> bool {
        match Self::from_dom(dom, self.pinned) {
            Some(summary) if summary != *self => {
                *self = summary;
                true
            }
            _ => false,
        }
    }

    fn retitle(&mut self, dom: &Dom) -> bool {
        let Some(root) = dom.root() else { return false };
        let title = title_of(dom, root);
        if title == self.title {
            return false;
        }
        self.title = title;
        true
    }
}

/// The `title` attribute, else the first line of the first user prompt, else [`UNTITLED`].
fn title_of(dom: &Dom, root: &Node) -> String {
    match root.str_attr(attr::TITLE).map(str::trim) {
        Some(t) if !t.is_empty() => t.to_owned(),
        _ => dom.iter().find(|e| is_user_message(e)).and_then(|prompt| derived_title(&prompt.text)).unwrap_or_else(|| UNTITLED.to_owned()),
    }
}

/// `element`'s `ended` time; `i64::MIN` without one.
fn ended_at(element: &Node) -> i64 { element.int_attr(attr::ENDED).unwrap_or(i64::MIN) }

fn is_waiting(element: &Node) -> bool { element.kind == ElementKind::Decision && element.str_attr(attr::STATUS) == Some(DecisionStatus::Waiting.as_str()) }

fn is_user_message(element: &Node) -> bool { element.kind == ElementKind::Message && element.str_attr(attr::ROLE) == Some(Role::User.as_str()) }

fn derived_title(prompt: &str) -> Option<String> {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(match line.char_indices().nth(DERIVED_TITLE_CHARS) {
        Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
        None => line.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, NodeId, Value};

    fn insert(dom: &mut Dom, parent: NodeId, kind: ElementKind, attrs: Vec<(&str, Value)>, text: &str) -> NodeId {
        dom.commit(Draft::insert(parent, kind, attrs, text), 1).expect("insert").node
    }

    fn base_attrs() -> Vec<(&'static str, Value)> {
        vec![(attr::ID, Value::from("s1")), (attr::HARNESS, Value::from("omp")), (attr::NODE, Value::from("key-laptop")), (attr::CREATED, Value::Int(100))]
    }

    /// Patch by patch, the incremental summary equals a fresh read.
    #[test]
    fn apply_matches_from_dom() {
        let mut dom = Dom::new();
        let mut summary: Option<SessionSummary> = None;
        let mut step = |dom: &mut Dom, draft: Draft, ts: i64| -> NodeId {
            let patch = dom.commit(draft, ts).expect("commit");
            match summary.as_mut() {
                Some(summary) => {
                    let before = summary.clone();
                    let changed = summary.apply(dom, &patch);
                    assert_eq!(changed, *summary != before, "change reported for {:?}", patch.op);
                }
                None => summary = SessionSummary::from_dom(dom, false),
            }
            assert_eq!(summary, SessionSummary::from_dom(dom, false), "after {:?}", patch.op);
            patch.node
        };
        let root = step(&mut dom, Draft::insert(NodeId::DOCUMENT, ElementKind::Session, base_attrs(), ""), 1);
        let turn = step(&mut dom, Draft::insert(root, ElementKind::Turn, vec![(attr::STATUS, Value::from("running")), (attr::STARTED, Value::Int(200))], ""), 2);
        let prompt = step(&mut dom, Draft::insert(turn, ElementKind::Message, vec![(attr::ROLE, Value::from("user"))], ""), 3);
        step(&mut dom, Draft::append(prompt, "Ship the release"), 4);
        let reply = step(&mut dom, Draft::insert(turn, ElementKind::Message, vec![(attr::ROLE, Value::from("assistant"))], "On it"), 5);
        step(&mut dom, Draft::append(reply, ", reading the changelog"), 6);
        let decision = step(&mut dom, Draft::insert(turn, ElementKind::Decision, vec![(attr::KIND, Value::from("approval")), (attr::STATUS, Value::from("waiting"))], ""), 7);
        step(&mut dom, Draft::set(decision, attr::STATUS, Value::from("answered")), 8);
        step(&mut dom, Draft::set(turn, attr::ENDED, Value::Int(900)), 9);
        step(&mut dom, Draft::set(turn, attr::STATUS, Value::from("done")), 10);
        step(&mut dom, Draft::set(root, attr::TITLE, Value::from("Release")), 11);
        step(&mut dom, Draft::remove(decision), 12);
        // Clearing the latest end falls back to `created`; the turn's start never counts.
        step(&mut dom, Draft::clear(turn, attr::ENDED), 13);

        let summary = summary.expect("summary");
        assert_eq!((summary.title.as_str(), summary.waiting, summary.updated), ("Release", false, 100));
    }

    #[test]
    fn untitled_session_takes_first_prompt_line_and_waiting_decision() {
        let mut dom = Dom::new();
        let root = insert(&mut dom, NodeId::DOCUMENT, ElementKind::Session, base_attrs(), "");
        let turn = insert(&mut dom, root, ElementKind::Turn, vec![(attr::STATUS, Value::from("waiting")), (attr::STARTED, Value::Int(500))], "");
        let user = vec![(attr::ROLE, Value::from("user")), (attr::STATUS, Value::from("done"))];
        insert(&mut dom, turn, ElementKind::Message, user, "\n  Fix the flaky deploy test  \nmore detail");
        let call_attrs = vec![(attr::TOOL, Value::from("bash")), (attr::CALL_ID, Value::from("c1")), (attr::STATUS, Value::from("running")), (attr::STARTED, Value::Int(900))];
        let call = insert(&mut dom, turn, ElementKind::Call, call_attrs, "");
        insert(&mut dom, call, ElementKind::Decision, vec![(attr::KIND, Value::from("approval")), (attr::STATUS, Value::from("waiting"))], "");

        let summary = SessionSummary::from_dom(&dom, true).expect("summary");
        assert_eq!(summary.title, "Fix the flaky deploy test");
        assert!(summary.waiting);
        assert!(summary.pinned);
        // Started work does not count: only `created` and `ended` move the session.
        assert_eq!(summary.updated, 100);
        assert_eq!(summary.node, "key-laptop");
    }

    #[test]
    fn answered_decision_is_not_waiting_and_title_attribute_wins() {
        let mut dom = Dom::new();
        let mut attrs = base_attrs();
        attrs.push((attr::TITLE, Value::from("Release notes")));
        attrs.push((attr::PROJECT, Value::from("loa")));
        let root = insert(&mut dom, NodeId::DOCUMENT, ElementKind::Session, attrs, "");
        let turn = insert(&mut dom, root, ElementKind::Turn, vec![(attr::STATUS, Value::from("running"))], "");
        insert(&mut dom, turn, ElementKind::Message, vec![(attr::ROLE, Value::from("user")), (attr::STATUS, Value::from("done"))], "write the notes");
        insert(&mut dom, turn, ElementKind::Decision, vec![(attr::KIND, Value::from("question")), (attr::STATUS, Value::from("answered"))], "");

        let summary = SessionSummary::from_dom(&dom, false).expect("summary");
        assert_eq!(summary.title, "Release notes");
        assert_eq!(summary.project.as_deref(), Some("loa"));
        assert!(!summary.waiting);
        assert_eq!(summary.updated, 100);
    }

    #[test]
    fn long_prompt_title_is_cut_on_a_char_boundary() {
        let title = derived_title(&"é".repeat(80)).expect("title");
        assert_eq!(title.chars().count(), DERIVED_TITLE_CHARS + 1);
        assert!(title.ends_with('…'));
    }
}
