//! Flattening of the session DOM into transcript items: one virtual-list row each.

use loa_core::{Dom, ElementKind, MessageKind, Node, NodeId, Role, attr};

/// `type` of the `<ext>` where adapters record the harness's slash commands: composer data,
/// not a transcript row.
pub const COMMANDS_EXT: &str = "available_commands_update";

/// What one transcript row shows; keys the [`super::rows`] renderer registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ItemKind { User, Assistant, Thinking, System, Calls, Decision, Job, Todo, Surface, Compaction, Ext }

/// One row: `key` is the node it starts at (a call group's first call), `top` the child of
/// `<session>` it belongs to, `calls` every call of a call group.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub key: NodeId,
    pub top: NodeId,
    pub kind: ItemKind,
    pub calls: Vec<NodeId>,
}

/// Appends the rows of a subtree, grouping consecutive calls of one tool, and reports every
/// visited node's row key through `own(node, key)`.
pub(crate) struct Emitter<'a, F: FnMut(NodeId, NodeId)> {
    pub dom: &'a Dom,
    pub top: NodeId,
    pub own: F,
}

impl<F: FnMut(NodeId, NodeId)> Emitter<'_, F> {
    pub(crate) fn emit(&mut self, out: &mut Vec<Item>, node: &Node) {
        let kind = match node.kind {
            ElementKind::Turn => return self.children(out, node),
            ElementKind::Call => return self.call(out, node),
            ElementKind::Message => message_kind(node),
            ElementKind::Decision => ItemKind::Decision,
            ElementKind::Job => ItemKind::Job,
            ElementKind::Todo => ItemKind::Todo,
            ElementKind::Surface => ItemKind::Surface,
            ElementKind::Compaction => ItemKind::Compaction,
            ElementKind::Ext if node.str_attr(attr::TYPE) != Some(COMMANDS_EXT) => ItemKind::Ext,
            _ => return,
        };
        out.push(Item { key: node.id, top: self.top, kind, calls: Vec::new() });
        (self.own)(node.id, node.id);
        self.children(out, node);
    }

    fn children(&mut self, out: &mut Vec<Item>, node: &Node) {
        for child in self.dom.children(node.id) {
            self.emit(out, child);
        }
    }

    fn call(&mut self, out: &mut Vec<Item>, node: &Node) {
        let tool = node.str_attr(attr::TOOL);
        let dom = self.dom;
        let key = match out.last_mut() {
            Some(last) if last.kind == ItemKind::Calls && last.top == self.top && dom.get(last.calls[0]).and_then(|f| f.str_attr(attr::TOOL)) == tool => {
                last.calls.push(node.id);
                last.key
            }
            _ => {
                out.push(Item { key: node.id, top: self.top, kind: ItemKind::Calls, calls: vec![node.id] });
                node.id
            }
        };
        (self.own)(node.id, key);
        for child in dom.children(node.id) {
            match child.kind {
                ElementKind::Input | ElementKind::Result | ElementKind::Usage => dom.descendants(child.id).for_each(|part| (self.own)(part.id, key)),
                _ => self.emit(out, child),
            }
        }
    }
}

fn message_kind(node: &Node) -> ItemKind {
    match node.str_attr(attr::ROLE).and_then(Role::parse) {
        Some(Role::User) => ItemKind::User,
        Some(Role::System) => ItemKind::System,
        _ if node.str_attr(attr::KIND).and_then(MessageKind::parse) == Some(MessageKind::Thinking) => ItemKind::Thinking,
        _ => ItemKind::Assistant,
    }
}

/// The child of `<session>` holding `id`; `None` for the session itself or a missing node.
pub(crate) fn top_of(dom: &Dom, id: NodeId) -> Option<NodeId> {
    let mut top = dom.get(id).filter(|n| n.kind != ElementKind::Session)?;
    for ancestor in dom.ancestors(id) {
        if ancestor.kind == ElementKind::Session {
            return Some(top.id);
        }
        top = ancestor;
    }
    None
}
