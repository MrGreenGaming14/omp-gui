//! The `<digest>` element as typed data.
//!
//! A session keeps at most one `<digest>` under its `<session>`. It records the
//! patch sequence it was built from (`source_seq`) and how many patches its own
//! write took ([`OWN_PATCHES`]). It is stale when the session has patches
//! beyond those, or when `stale` was set explicitly.

use loa_core::{Dom, Draft, ElementKind, Node, NodeId, Value, attr};

/// Extension attribute on `<digest>`: number of patches the write that
/// produced the digest committed (removing the old digest plus inserting the
/// new one). Patches past `source_seq + x-own_patches` are someone else's.
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
        match text.split_once(": ") {
            Some((path, note)) => Self { path: path.to_owned(), note: note.to_owned() },
            None => Self { path: text.to_owned(), note: String::new() },
        }
    }

    /// The schema's `"path: note"` form.
    pub fn encode(&self) -> String {
        if self.note.is_empty() { self.path.clone() } else { format!("{}: {}", self.path, self.note) }
    }
}

/// What a digest says about its session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DigestContent {
    /// Two or three sentences on what the session did.
    pub summary: String,
    pub decisions: Vec<String>,
    pub files: Vec<FileNote>,
    /// Unfinished todos and unanswered questions.
    pub open_items: Vec<String>,
    /// Node ids of the turns the digest came from.
    pub pointers: Vec<NodeId>,
}

/// A digest read from a session DOM.
#[derive(Clone, Debug, PartialEq)]
pub struct Digest {
    /// The `<digest>` node.
    pub node: NodeId,
    pub content: DigestContent,
    /// Patch sequence the digest was built from.
    pub source_seq: u64,
    /// Whether it is out of date: flagged stale, or the session moved on.
    pub stale: bool,
    pub model: Option<String>,
    pub created: Option<i64>,
}

/// Where a session's digest stands.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DigestState {
    Missing,
    Fresh,
    Stale,
}

fn digest_node(dom: &Dom) -> Option<&Node> {
    let root = dom.root()?;
    dom.children_of_kind(root.id, ElementKind::Digest).last()
}

fn is_node_stale(dom: &Dom, node: &Node) -> bool {
    let source = node.int_attr(attr::SOURCE_SEQ).unwrap_or(0).max(0) as u64;
    let own = node.int_attr(OWN_PATCHES).unwrap_or(0).max(0) as u64;
    node.bool_attr(attr::STALE) == Some(true) || dom.seq() > source.saturating_add(own)
}

/// The session's digest, if it has one.
pub fn digest_of(dom: &Dom) -> Option<Digest> {
    let node = digest_node(dom)?;
    let list = |name: &str| -> Vec<String> {
        node.attr(name).map(|v| v.str_items().map(str::to_owned).collect()).unwrap_or_default()
    };
    let pointers = node
        .attr(attr::POINTERS)
        .and_then(Value::as_list)
        .unwrap_or_default()
        .iter()
        .filter_map(Value::as_int)
        .map(|id| NodeId(id.max(0) as u64))
        .collect();
    Some(Digest {
        node: node.id,
        content: DigestContent {
            summary: node.text.clone(),
            decisions: list(attr::DECISIONS),
            files: list(attr::FILES).iter().map(|f| FileNote::parse(f)).collect(),
            open_items: list(attr::OPEN_ITEMS),
            pointers,
        },
        source_seq: node.int_attr(attr::SOURCE_SEQ).unwrap_or(0).max(0) as u64,
        stale: is_node_stale(dom, node),
        model: node.str_attr(attr::MODEL).map(str::to_owned),
        created: node.int_attr(attr::CREATED),
    })
}

/// Whether the session has a digest and whether it is current.
pub fn digest_state(dom: &Dom) -> DigestState {
    match digest_node(dom) {
        None => DigestState::Missing,
        Some(node) if is_node_stale(dom, node) => DigestState::Stale,
        Some(_) => DigestState::Fresh,
    }
}

/// The drafts that replace the session's digest with `content`, built from
/// the DOM at `source_seq`. Commit them in order, right after each other.
/// Returns nothing when the DOM has no `<session>` root.
pub fn write_drafts(
    dom: &Dom,
    content: &DigestContent,
    source_seq: u64,
    model: Option<&str>,
    created: i64,
) -> Vec<Draft> {
    let Some(root) = dom.root() else { return Vec::new() };
    let mut drafts: Vec<Draft> =
        dom.children_of_kind(root.id, ElementKind::Digest).map(|d| Draft::remove(d.id)).collect();
    let own = drafts.len() as i64 + 1;
    let strings = |items: &[String]| Value::List(items.iter().cloned().map(Value::Str).collect());
    let mut attrs = vec![
        (attr::SOURCE_SEQ, Value::Int(i64::try_from(source_seq).unwrap_or(i64::MAX))),
        (OWN_PATCHES, Value::Int(own)),
        (attr::DECISIONS, strings(&content.decisions)),
        (attr::FILES, Value::List(content.files.iter().map(|f| Value::Str(f.encode())).collect())),
        (attr::OPEN_ITEMS, strings(&content.open_items)),
        (
            attr::POINTERS,
            Value::List(content.pointers.iter().map(|p| Value::Int(i64::try_from(p.0).unwrap_or(i64::MAX))).collect()),
        ),
        (attr::CREATED, Value::Int(created)),
    ];
    if let Some(model) = model {
        attrs.push((attr::MODEL, Value::from(model)));
    }
    drafts.push(Draft::insert(root.id, ElementKind::Digest, attrs, content.summary.clone()));
    drafts
}

/// The draft that flags the digest stale, when it has one that isn't flagged.
/// Used when a rebuild fails, so the old digest stays but reads as outdated.
pub fn mark_stale_draft(dom: &Dom) -> Option<Draft> {
    let node = digest_node(dom)?;
    (node.bool_attr(attr::STALE) != Some(true)).then(|| Draft::set(node.id, attr::STALE, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::testing::Builder;
    use loa_core::PatchSink;

    fn content() -> DigestContent {
        DigestContent {
            summary: "Picked iced for the UI.".into(),
            decisions: vec!["iced over GPUI".into()],
            files: vec![FileNote { path: "src/ui.rs".into(), note: "window setup".into() }],
            open_items: vec!["theme switch".into()],
            pointers: vec![NodeId(2)],
        }
    }

    fn write(b: &Builder, source_seq: u64) {
        for draft in write_drafts(&b.dom(), &content(), source_seq, Some("smol"), 5) {
            b.sink.commit(draft).unwrap();
        }
    }

    #[test]
    fn a_written_digest_reads_back_and_is_fresh() {
        let b = Builder::new("s1", "p", 1);
        b.turn("hi", "hello", 2);
        write(&b, b.dom().seq());
        let dom = b.dom();
        let digest = digest_of(&dom).unwrap();
        assert_eq!(digest.content, content());
        assert_eq!(digest.model.as_deref(), Some("smol"));
        assert!(!digest.stale);
        assert_eq!(digest_state(&dom), DigestState::Fresh);
    }

    #[test]
    fn replacing_a_digest_keeps_one_and_stays_fresh() {
        let b = Builder::new("s1", "p", 1);
        b.turn("hi", "hello", 2);
        write(&b, b.dom().seq());
        b.turn("more", "sure", 3);
        assert_eq!(digest_state(&b.dom()), DigestState::Stale);
        write(&b, b.dom().seq());
        let dom = b.dom();
        assert_eq!(dom.children_of_kind(dom.root().unwrap().id, ElementKind::Digest).count(), 1);
        assert_eq!(digest_state(&dom), DigestState::Fresh);
    }

    #[test]
    fn a_patch_after_the_source_marks_it_stale() {
        let b = Builder::new("s1", "p", 1);
        let turn = b.turn("hi", "hello", 2);
        write(&b, b.dom().seq());
        b.commit(Draft::set(turn, attr::STATUS, "error"));
        assert!(digest_of(&b.dom()).unwrap().stale);
    }

    #[test]
    fn a_patch_between_build_and_write_counts_as_stale() {
        let b = Builder::new("s1", "p", 1);
        let turn = b.turn("hi", "hello", 2);
        let built_at = b.dom().seq();
        b.commit(Draft::set(turn, attr::STATUS, "error"));
        write(&b, built_at);
        assert_eq!(digest_state(&b.dom()), DigestState::Stale);
    }

    #[test]
    fn mark_stale_flags_once() {
        let b = Builder::new("s1", "p", 1);
        b.turn("hi", "hello", 2);
        assert_eq!(digest_state(&b.dom()), DigestState::Missing);
        write(&b, b.dom().seq());
        b.sink.commit(mark_stale_draft(&b.dom()).unwrap()).unwrap();
        assert_eq!(digest_state(&b.dom()), DigestState::Stale);
        assert!(mark_stale_draft(&b.dom()).is_none());
    }
}
