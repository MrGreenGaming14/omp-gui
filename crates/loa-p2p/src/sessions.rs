//! The port to this node's own sessions, which peers may view and drive.

use loa_bus::{Bus, SessionWriter};
use loa_core::{Dom, ElementKind, NodeId, attr};
use loa_trust::SessionFacts;
use std::sync::Arc;

/// Characters of a prompt shown when picking it as a grant resource.
const PREVIEW_CHARS: usize = 60;

/// One prompt (`<turn>`) of a local session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalTurn {
    pub id: NodeId,
    /// The start of the user's message, for pickers.
    pub preview: String,
}

/// A session this node owns, as grants and peers see it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalSession {
    pub facts: SessionFacts,
    pub title: String,
    pub harness: String,
    pub last_seq: u64,
    pub turns: Vec<LocalTurn>,
}

impl LocalSession {
    /// Reads a session DOM; `None` when it has no root with an id and creation time.
    pub fn from_dom(dom: &Dom) -> Option<Self> {
        let facts = SessionFacts::from_dom(dom)?;
        let root = dom.root()?;
        let turns = dom
            .children_of_kind(root.id, ElementKind::Turn)
            .map(|turn| LocalTurn { id: turn.id, preview: preview(dom, turn.id) })
            .collect();
        Some(Self {
            title: root.str_attr(attr::TITLE).unwrap_or_default().to_owned(),
            harness: root.str_attr(attr::HARNESS).unwrap_or_default().to_owned(),
            last_seq: dom.seq(),
            facts,
            turns,
        })
    }

    /// The title, or the first prompt when the session has none yet.
    pub fn label(&self) -> String {
        if !self.title.is_empty() {
            return self.title.clone();
        }
        self.turns.first().map(|t| t.preview.clone()).filter(|p| !p.is_empty()).unwrap_or_else(|| self.facts.id.clone())
    }
}

fn preview(dom: &Dom, turn: NodeId) -> String {
    let text = dom
        .descendants(turn)
        .find(|n| n.kind == ElementKind::Message && n.str_attr(attr::ROLE) == Some("user"))
        .map(|n| n.text.trim())
        .unwrap_or_default();
    let mut out: String = text.chars().take(PREVIEW_CHARS).collect();
    if text.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    out
}

/// What the node needs from the app's sessions. `loa-app` backs it with its
/// session registry, which opens sessions lazily from the journal store.
pub trait LocalSessions: Send + Sync {
    /// Every session this node owns that grants can name.
    fn sessions(&self) -> Vec<LocalSession>;
    /// The live writer of a session this node owns, opening it if needed.
    /// `None` for unknown sessions and for mirrors of other nodes' sessions.
    fn writer(&self, id: &str) -> Option<Arc<SessionWriter>>;
}

/// The sessions open on a [`Bus`] that this node writes.
pub struct BusSessions {
    bus: Arc<Bus>,
}

impl BusSessions {
    pub fn new(bus: Arc<Bus>) -> Self {
        Self { bus }
    }
}

impl LocalSessions for BusSessions {
    fn sessions(&self) -> Vec<LocalSession> {
        let mut out: Vec<LocalSession> = self
            .bus
            .ids()
            .into_iter()
            .filter_map(|id| self.bus.get(id.as_str()))
            .filter(|writer| writer.is_writer())
            .filter_map(|writer| writer.with_dom(LocalSession::from_dom))
            .collect();
        out.sort_by(|a, b| b.facts.created_ms.cmp(&a.facts.created_ms).then_with(|| a.facts.id.cmp(&b.facts.id)));
        out
    }

    fn writer(&self, id: &str) -> Option<Arc<SessionWriter>> {
        self.bus.get(id).filter(|writer| writer.is_writer())
    }
}
