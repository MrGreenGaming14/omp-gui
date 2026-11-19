//! Flattening of the session DOM into transcript items: one row of the
//! virtual list each.

use loa_core::{Dom, ElementKind, MessageKind, Node, NodeId, Role, attr};

/// `type` of the `<ext>` where adapters record the harness's slash commands
/// (omp's and ACP's `available_commands_update`). Session data the
/// composer offers, not a transcript row.
pub const COMMANDS_EXT: &str = "available_commands_update";

/// What one transcript row shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemKind {
    /// A user prompt. Clicking it asks for a rewind.
    User,
    /// Assistant text, rendered as markdown.
    Assistant,
    /// Assistant thinking, collapsed and muted.
    Thinking,
    /// A system message.
    System,
    /// One or more consecutive calls of the same tool.
    Calls,
    /// A `<decision>`: a slot the app fills with a decision card.
    Decision,
    Job,
    Todo,
    Surface,
    Compaction,
    Ext,
}

/// One row of the transcript.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Item {
    /// The node the row starts at. For a call group, its first call.
    pub key: NodeId,
    /// The child of `<session>` the row belongs to.
    pub top: NodeId,
    pub kind: ItemKind,
    /// Every call of a call group, in order. Empty for other kinds.
    pub calls: Vec<NodeId>,
}

/// Appends the rows of a subtree to `out`, grouping consecutive calls of the
/// same tool, and reports the owning row of every node it visits.
pub(crate) struct Emitter<'a, F: FnMut(NodeId, NodeId)> {
    pub dom: &'a Dom,
    pub top: NodeId,
    /// Called with `(node, row key)` for every node that belongs to a row.
    pub own: F,
}

impl<F: FnMut(NodeId, NodeId)> Emitter<'_, F> {
    /// Emits `node` and everything under it.
    pub(crate) fn emit(&mut self, out: &mut Vec<Item>, node: &Node) {
        match node.kind {
            ElementKind::Session => {}
            ElementKind::Turn => {
                for child in self.dom.children(node.id) {
                    self.emit(out, child);
                }
            }
            ElementKind::Call => self.emit_call(out, node),
            ElementKind::Message => {
                let kind = message_kind(node);
                self.row(out, node, kind);
                self.emit_children(out, node);
            }
            ElementKind::Decision => self.leaf(out, node, ItemKind::Decision),
            ElementKind::Job => self.leaf(out, node, ItemKind::Job),
            ElementKind::Todo => self.leaf(out, node, ItemKind::Todo),
            ElementKind::Surface => self.leaf(out, node, ItemKind::Surface),
            ElementKind::Compaction => self.leaf(out, node, ItemKind::Compaction),
            ElementKind::Ext if node.str_attr(attr::TYPE) == Some(COMMANDS_EXT) => {}
            ElementKind::Ext => self.leaf(out, node, ItemKind::Ext),
            // Parts of a call, or data the transcript doesn't show.
            ElementKind::Input | ElementKind::Result | ElementKind::Usage | ElementKind::Digest => {
            }
        }
    }

    fn emit_call(&mut self, out: &mut Vec<Item>, node: &Node) {
        let tool = node.str_attr(attr::TOOL).unwrap_or_default();
        let joined = match out.last_mut() {
            Some(last) if last.kind == ItemKind::Calls && last.top == self.top => {
                let same_tool = last
                    .calls
                    .first()
                    .and_then(|first| self.dom.get(*first))
                    .and_then(|first| first.str_attr(attr::TOOL))
                    == Some(tool);
                if same_tool {
                    last.calls.push(node.id);
                    Some(last.key)
                } else {
                    None
                }
            }
            _ => None,
        };
        let key = match joined {
            Some(key) => key,
            None => {
                out.push(Item {
                    key: node.id,
                    top: self.top,
                    kind: ItemKind::Calls,
                    calls: vec![node.id],
                });
                node.id
            }
        };
        (self.own)(node.id, key);
        for child in self.dom.children(node.id) {
            match child.kind {
                ElementKind::Input | ElementKind::Result | ElementKind::Usage => {
                    for part in self.dom.descendants(child.id) {
                        (self.own)(part.id, key);
                    }
                }
                _ => self.emit(out, child),
            }
        }
    }

    fn row(&mut self, out: &mut Vec<Item>, node: &Node, kind: ItemKind) {
        out.push(Item {
            key: node.id,
            top: self.top,
            kind,
            calls: Vec::new(),
        });
        (self.own)(node.id, node.id);
    }

    fn leaf(&mut self, out: &mut Vec<Item>, node: &Node, kind: ItemKind) {
        self.row(out, node, kind);
        self.emit_children(out, node);
    }

    fn emit_children(&mut self, out: &mut Vec<Item>, node: &Node) {
        for child in self.dom.children(node.id) {
            self.emit(out, child);
        }
    }
}

fn message_kind(node: &Node) -> ItemKind {
    let role = node.str_attr(attr::ROLE).and_then(Role::parse);
    let thinking =
        node.str_attr(attr::KIND).and_then(MessageKind::parse) == Some(MessageKind::Thinking);
    match role {
        Some(Role::User) => ItemKind::User,
        Some(Role::System) => ItemKind::System,
        Some(Role::Assistant) | None if thinking => ItemKind::Thinking,
        Some(Role::Assistant) | None => ItemKind::Assistant,
    }
}

/// The child of `<session>` that holds `id`, or `None` when `id` is the
/// session itself or not in the DOM.
pub(crate) fn top_of(dom: &Dom, id: NodeId) -> Option<NodeId> {
    let node = dom.get(id)?;
    if node.kind == ElementKind::Session {
        return None;
    }
    let mut top = node;
    for ancestor in dom.ancestors(id) {
        if ancestor.kind == ElementKind::Session {
            return Some(top.id);
        }
        top = ancestor;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::{Draft, Value};

    fn session(dom: &mut Dom) -> NodeId {
        let attrs: [(&str, Value); 4] = [
            ("id", "s".into()),
            ("harness", "omp".into()),
            ("node", "n".into()),
            ("created", Value::Int(0)),
        ];
        dom.commit(
            Draft::insert(NodeId::DOCUMENT, ElementKind::Session, attrs, ""),
            0,
        )
        .unwrap()
        .node
    }

    fn insert(dom: &mut Dom, parent: NodeId, kind: ElementKind, attrs: &[(&str, &str)]) -> NodeId {
        let attrs = attrs.iter().map(|(k, v)| (*k, Value::from(*v)));
        dom.commit(Draft::insert(parent, kind, attrs, ""), 0)
            .unwrap()
            .node
    }

    fn flatten(dom: &Dom, top: NodeId) -> Vec<Item> {
        let mut out = Vec::new();
        let mut emitter = Emitter {
            dom,
            top,
            own: |_, _| {},
        };
        emitter.emit(&mut out, dom.get(top).unwrap());
        out
    }

    #[test]
    fn consecutive_calls_of_one_tool_share_a_row_and_other_rows_split_them() {
        let mut dom = Dom::new();
        let s = session(&mut dom);
        let turn = insert(&mut dom, s, ElementKind::Turn, &[]);
        let r1 = insert(&mut dom, turn, ElementKind::Call, &[("tool", "read")]);
        let r2 = insert(&mut dom, turn, ElementKind::Call, &[("tool", "read")]);
        let r3 = insert(&mut dom, turn, ElementKind::Call, &[("tool", "read")]);
        let b1 = insert(&mut dom, turn, ElementKind::Call, &[("tool", "bash")]);
        let text = insert(
            &mut dom,
            turn,
            ElementKind::Message,
            &[("role", "assistant")],
        );
        let r4 = insert(&mut dom, turn, ElementKind::Call, &[("tool", "read")]);
        let rows = flatten(&dom, turn);
        let shape: Vec<(ItemKind, Vec<NodeId>)> =
            rows.iter().map(|i| (i.kind, i.calls.clone())).collect();
        assert_eq!(
            shape,
            vec![
                (ItemKind::Calls, vec![r1, r2, r3]),
                (ItemKind::Calls, vec![b1]),
                (ItemKind::Assistant, vec![]),
                (ItemKind::Calls, vec![r4]),
            ]
        );
        assert_eq!(rows[2].key, text);
    }

    #[test]
    fn a_decision_under_a_call_follows_the_call_row() {
        let mut dom = Dom::new();
        let s = session(&mut dom);
        let turn = insert(&mut dom, s, ElementKind::Turn, &[]);
        let call = insert(&mut dom, turn, ElementKind::Call, &[("tool", "bash")]);
        insert(&mut dom, call, ElementKind::Input, &[]);
        let decision = insert(
            &mut dom,
            call,
            ElementKind::Decision,
            &[("kind", "approval")],
        );
        let kinds: Vec<(ItemKind, NodeId)> = flatten(&dom, turn)
            .iter()
            .map(|i| (i.kind, i.key))
            .collect();
        assert_eq!(
            kinds,
            vec![(ItemKind::Calls, call), (ItemKind::Decision, decision)]
        );
    }
}
