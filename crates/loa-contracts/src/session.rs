//! What a session is at a glance: the summary lists and title bars show.

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
    /// Reads a `<session id node>` DOM: title attr or first user prompt line; `updated` is the latest timestamp.
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
