//! `project_read`: the subtree behind a search hit or digest pointer, as indented text in byte pages.

use crate::search::render_value;
use crate::source::{Access, SessionSource, open_visible};
use crate::{Error, Result};
use loa_core::{Dom, NodeId};
use loa_util::text::Paginator;
use std::fmt::Write as _;

/// Page sizes in bytes.
pub const PAGES: Paginator = Paginator { default: 8_000, max: 32_000 };

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadRequest {
    pub session: String,
    /// Root of the subtree; `None` reads the whole session.
    pub node: Option<NodeId>,
    /// Byte offset into the rendered text.
    pub offset: usize,
    /// See [`PAGES`].
    pub max_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadPage {
    pub session: String,
    pub node: NodeId,
    pub text: String,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub total_bytes: usize,
}

/// A missing, other-project or denied session all give [`Error::SessionNotFound`].
pub async fn read_subtree(source: &dyn SessionSource, access: &dyn Access, project: &str, request: &ReadRequest) -> Result<ReadPage> {
    let dom = open_visible(source, access, project, &request.session).await?;
    let node = request.node.or(dom.root().map(|r| r.id)).ok_or_else(|| Error::SessionNotFound(request.session.clone()))?;
    if !dom.contains(node) {
        return Err(Error::NodeNotFound { session: request.session.clone(), node: node.0 });
    }
    let rendered = render_subtree(&dom, node);
    let (range, next_offset) = PAGES.bytes(&rendered, request.offset, request.max_bytes);
    let (offset, text) = (range.start, rendered[range].to_owned());
    Ok(ReadPage { session: request.session.clone(), node, text, offset, next_offset, total_bytes: rendered.len() })
}

/// `kind #id attr=value …` lines, each node's text one level deeper.
pub fn render_subtree(dom: &Dom, node: NodeId) -> String {
    let mut out = String::new();
    let base = dom.ancestors(node).count();
    for n in dom.descendants(node) {
        let indent = "  ".repeat(dom.ancestors(n.id).count() - base);
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
