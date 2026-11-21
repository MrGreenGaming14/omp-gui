//! The `<digest>` element as typed data. A session keeps at most one, under its `<session>`.
//! It records the sequence it was built from and how many patches its own write took, and
//! is stale when the session has patches beyond those or `stale` is set.

use loa_core::{Dom, Draft, ElementKind, Node, NodeId, Value, attr};

/// Extension attribute: patches the digest's own write committed (old removed, new inserted).
pub const OWN_PATCHES: &str = "x-own_patches";

/// One touched file and a one line note.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileNote {
    pub path: String,
    #[serde(default)]
    pub note: String,
}

impl FileNote {
    /// Parses the schema's `"path: note"` form.
    pub fn parse(text: &str) -> Self {
        let (path, note) = text.split_once(": ").unwrap_or((text, ""));
        Self { path: path.to_owned(), note: note.to_owned() }
    }

    pub fn encode(&self) -> String {
        if self.note.is_empty() { self.path.clone() } else { format!("{}: {}", self.path, self.note) }
    }
}

/// What a digest says about its session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DigestContent {
    pub summary: String,
    pub decisions: Vec<String>,
    pub files: Vec<FileNote>,
    /// Unfinished todos and unanswered questions.
    pub open_items: Vec<String>,
    /// The turns the digest came from.
    pub pointers: Vec<NodeId>,
}

/// A digest read from a session DOM.
#[derive(Clone, Debug, PartialEq)]
pub struct Digest {
    pub node: NodeId,
    pub content: DigestContent,
    pub source_seq: u64,
    /// Flagged stale, or the session moved on.
    pub stale: bool,
    pub model: Option<String>,
    pub created: Option<i64>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DigestState {
    Missing,
    Fresh,
    Stale,
}

fn digest_node(dom: &Dom) -> Option<&Node> {
    dom.children_of_kind(dom.root()?.id, ElementKind::Digest).last()
}

fn uint(node: &Node, name: &str) -> u64 {
    node.int_attr(name).unwrap_or(0).max(0) as u64
}

fn is_stale(dom: &Dom, node: &Node) -> bool {
    node.bool_attr(attr::STALE) == Some(true) || dom.seq() > uint(node, attr::SOURCE_SEQ).saturating_add(uint(node, OWN_PATCHES))
}

pub fn digest_of(dom: &Dom) -> Option<Digest> {
    let node = digest_node(dom)?;
    let list = |name: &str| -> Vec<String> { node.attr(name).map(|v| v.str_items().map(str::to_owned).collect()).unwrap_or_default() };
    let pointers = node.attr(attr::POINTERS).and_then(Value::as_list).unwrap_or_default();
    Some(Digest {
        node: node.id,
        content: DigestContent {
            summary: node.text.clone(),
            decisions: list(attr::DECISIONS),
            files: list(attr::FILES).iter().map(|f| FileNote::parse(f)).collect(),
            open_items: list(attr::OPEN_ITEMS),
            pointers: pointers.iter().filter_map(Value::as_int).map(|id| NodeId(id.max(0) as u64)).collect(),
        },
        source_seq: uint(node, attr::SOURCE_SEQ),
        stale: is_stale(dom, node),
        model: node.str_attr(attr::MODEL).map(str::to_owned),
        created: node.int_attr(attr::CREATED),
    })
}

pub fn digest_state(dom: &Dom) -> DigestState {
    match digest_node(dom) {
        None => DigestState::Missing,
        Some(node) if is_stale(dom, node) => DigestState::Stale,
        Some(_) => DigestState::Fresh,
    }
}

/// The drafts that replace the digest with `content` built at `source_seq`; commit them
/// in order, back to back. Empty without a `<session>` root.
pub fn write_drafts(dom: &Dom, content: &DigestContent, source_seq: u64, model: Option<&str>, created: i64) -> Vec<Draft> {
    let Some(root) = dom.root() else { return Vec::new() };
    let mut drafts: Vec<Draft> = dom.children_of_kind(root.id, ElementKind::Digest).map(|d| Draft::remove(d.id)).collect();
    let int = |n: u64| Value::Int(i64::try_from(n).unwrap_or(i64::MAX));
    let strings = |items: &mut dyn Iterator<Item = String>| Value::List(items.map(Value::Str).collect());
    let mut attrs = vec![
        (attr::SOURCE_SEQ, int(source_seq)),
        (OWN_PATCHES, int(drafts.len() as u64 + 1)),
        (attr::DECISIONS, strings(&mut content.decisions.iter().cloned())),
        (attr::FILES, strings(&mut content.files.iter().map(FileNote::encode))),
        (attr::OPEN_ITEMS, strings(&mut content.open_items.iter().cloned())),
        (attr::POINTERS, Value::List(content.pointers.iter().map(|p| int(p.0)).collect())),
        (attr::CREATED, Value::Int(created)),
    ];
    attrs.extend(model.map(|m| (attr::MODEL, Value::from(m))));
    drafts.push(Draft::insert(root.id, ElementKind::Digest, attrs, content.summary.clone()));
    drafts
}

/// Flags the digest stale (after a failed rebuild), unless it already is.
pub fn mark_stale_draft(dom: &Dom) -> Option<Draft> {
    let node = digest_node(dom)?;
    (node.bool_attr(attr::STALE) != Some(true)).then(|| Draft::set(node.id, attr::STALE, true))
}
