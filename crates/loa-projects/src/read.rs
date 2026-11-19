//! `project_read`: the full subtree behind a search hit or digest pointer,
//! rendered as compact indented text and served in pages.

use crate::search::render_value;
use crate::source::{Access, Requester, SessionSource, open_visible};
use crate::text::floor_boundary;
use crate::{Error, Result};
use loa_core::{Dom, NodeId};
use std::fmt::Write as _;

/// Page size when the request doesn't say.
pub const DEFAULT_PAGE_BYTES: usize = 8_000;
/// Largest page.
pub const MAX_PAGE_BYTES: usize = 32_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    pub session: String,
    /// Root of the subtree; `None` reads the whole session.
    pub node: Option<NodeId>,
    /// Byte offset into the rendered text, from the previous page's `next_offset`.
    pub offset: usize,
    /// Page size in bytes, clamped to [`MAX_PAGE_BYTES`]; 0 means [`DEFAULT_PAGE_BYTES`].
    pub max_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadPage {
    pub session: String,
    pub node: NodeId,
    pub text: String,
    /// Where this page starts.
    pub offset: usize,
    pub next_offset: Option<usize>,
    /// Size of the whole rendered subtree.
    pub total_bytes: usize,
}

/// Reads a subtree of a session of `project` that `requester` may open. A
/// session that is missing, in another project or denied gives the same
/// [`Error::SessionNotFound`].
pub async fn read_subtree(
    source: &dyn SessionSource,
    access: &dyn Access,
    requester: &Requester,
    project: &str,
    request: &ReadRequest,
) -> Result<ReadPage> {
    let (_, dom) = open_visible(source, access, requester, project, &request.session).await?;
    let node = match request.node {
        Some(node) => node,
        None => dom.root().map(|r| r.id).ok_or_else(|| Error::SessionNotFound(request.session.clone()))?,
    };
    if !dom.contains(node) {
        return Err(Error::NodeNotFound { session: request.session.clone(), node: node.0 });
    }
    let rendered = render_subtree(&dom, node);
    let max = match request.max_bytes {
        0 => DEFAULT_PAGE_BYTES,
        n => n.min(MAX_PAGE_BYTES),
    };
    let start = floor_boundary(&rendered, request.offset);
    let mut end = floor_boundary(&rendered, start.saturating_add(max));
    if end < rendered.len() {
        // End on a line break when one is in the second half of the page.
        if let Some(nl) = rendered[start..end].rfind('\n')
            && nl > max / 2
        {
            end = start + nl + 1;
        }
        // A page always advances, even with a single huge char.
        if end == start {
            end = crate::text::ceil_boundary(&rendered, start + 1);
        }
    }
    Ok(ReadPage {
        session: request.session.clone(),
        node,
        text: rendered[start..end].to_owned(),
        offset: start,
        next_offset: (end < rendered.len()).then_some(end),
        total_bytes: rendered.len(),
    })
}

/// `node` and its subtree as indented lines: `kind #id attr=value …`, then
/// the node text indented one level deeper.
pub fn render_subtree(dom: &Dom, node: NodeId) -> String {
    let mut out = String::new();
    let base = dom.ancestors(node).count();
    for n in dom.descendants(node) {
        let depth = dom.ancestors(n.id).count() - base;
        let indent = "  ".repeat(depth);
        let _ = write!(out, "{indent}{} #{}", n.kind, n.id);
        for (name, value) in &n.attrs {
            let _ = write!(out, " {name}={}", render_value(value));
        }
        out.push('\n');
        for line in n.text.lines() {
            let _ = writeln!(out, "{indent}  {line}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::testing::{Builder, MemorySource, TestAccess, peer};

    fn project() -> (MemorySource, NodeId) {
        let a = Builder::new("a", "proj", 10);
        let t = a.turn("Line one\nline two", "Reply", 11);
        a.call(t, "edit", &format!("{{\"path\":\"src/net.rs\",\"body\":\"{}\"}}", "ü".repeat(3000)), "ok");
        a.turn("unrelated later turn", "", 12);
        let b = Builder::new("b", "other", 10);
        b.turn("other project", "", 11);
        (MemorySource::with([a.dom(), b.dom()]), t)
    }

    fn req(session: &str, node: Option<NodeId>, offset: usize, max_bytes: usize) -> ReadRequest {
        ReadRequest { session: session.into(), node, offset, max_bytes }
    }

    #[tokio::test]
    async fn reads_the_subtree_behind_a_pointer() {
        let (source, turn) = project();
        let page = read_subtree(&source, &TestAccess::default(), &Requester::Local, "proj", &req("a", Some(turn), 0, 32_000))
            .await
            .unwrap();
        assert!(page.text.starts_with(&format!("turn #{turn} ended=11 status=done\n  message #")));
        assert!(page.text.contains("role=user\n    Line one\n    line two\n"));
        assert!(page.text.contains("call #"));
        assert!(page.text.contains("tool=edit"));
        assert!(!page.text.contains("unrelated later turn"));
        assert_eq!(page.next_offset, None);
    }

    #[tokio::test]
    async fn pages_cover_the_text_exactly_once_on_char_boundaries() {
        let (source, turn) = project();
        let access = TestAccess::default();
        let whole = read_subtree(&source, &access, &Requester::Local, "proj", &req("a", Some(turn), 0, 32_000)).await.unwrap();
        let mut joined = String::new();
        let mut offset = 0;
        loop {
            let page = read_subtree(&source, &access, &Requester::Local, "proj", &req("a", Some(turn), offset, 1_001))
                .await
                .unwrap();
            assert!(page.text.len() <= 1_001);
            assert_eq!(page.offset, offset);
            joined.push_str(&page.text);
            match page.next_offset {
                Some(next) => offset = next,
                None => break,
            }
        }
        assert_eq!(joined, whole.text);
        assert_eq!(whole.total_bytes, whole.text.len());
    }

    #[tokio::test]
    async fn denied_other_project_and_missing_sessions_read_alike() {
        let (source, turn) = project();
        let access = TestAccess::only(&[]);
        let denied = read_subtree(&source, &access, &peer(), "proj", &req("a", Some(turn), 0, 0)).await.unwrap_err();
        let other = read_subtree(&source, &access, &Requester::Local, "proj", &req("b", None, 0, 0)).await.unwrap_err();
        let missing = read_subtree(&source, &access, &Requester::Local, "proj", &req("zz", None, 0, 0)).await.unwrap_err();
        for err in [denied, other, missing] {
            assert!(matches!(err, Error::SessionNotFound(_)), "{err}");
        }
        let bad_node = read_subtree(&source, &access, &Requester::Local, "proj", &req("a", Some(NodeId(999)), 0, 0)).await;
        assert!(matches!(bad_node, Err(Error::NodeNotFound { .. })));
    }
}
