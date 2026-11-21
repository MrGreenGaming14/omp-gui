//! The port to this node's own sessions, which peers may view and drive.

use loa_util::LockExt;
use loa_bus::{Bus, SessionWriter};
use loa_core::{Dom, ElementKind, NodeId, attr};
use loa_harness::features::library::Library;
use loa_trust::SessionFacts;
use loa_bus::Listing;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

/// Every session in the app's [`Library`], live or only journaled. Peers'
/// requests open a session's writer through the library, so its changes are
/// journaled like the user's own.
pub struct LibrarySessions {
    library: Arc<Library>,
    /// Sessions read from their journals, by id, with the sequence they were read at.
    cache: Mutex<HashMap<String, LocalSession>>,
}

impl LibrarySessions {
    pub fn new(library: Arc<Library>) -> Self {
        Self { library, cache: Mutex::default() }
    }

    /// `id` at journal sequence `seq`, read again only when the journal moved.
    fn closed(&self, id: &str, seq: u64) -> Option<LocalSession> {
        let mut cache = self.cache.locked();
        if let Some(session) = cache.get(id).filter(|s| s.last_seq == seq) {
            return Some(session.clone());
        }
        let session = LocalSession::from_dom(&self.library.load(id).ok()?)?;
        cache.insert(id.to_owned(), session.clone());
        Some(session)
    }
}

impl LocalSessions for LibrarySessions {
    fn sessions(&self) -> Vec<LocalSession> {
        let Ok(listings) = self.library.store().list() else { return Vec::new() };
        let mut out: Vec<LocalSession> = listings
            .into_iter()
            .filter_map(|listing| match listing {
                Listing::Session(info) => Some(info),
                Listing::Unreadable { .. } => None,
            })
            .filter_map(|info| match self.library.live(&info.id) {
                Some(live) => live.writer.is_writer().then(|| live.writer.with_dom(LocalSession::from_dom)).flatten(),
                None => self.closed(&info.id, info.last_seq),
            })
            .collect();
        out.sort_by(|a, b| b.facts.created_ms.cmp(&a.facts.created_ms).then_with(|| a.facts.id.cmp(&b.facts.id)));
        out
    }

    fn writer(&self, id: &str) -> Option<Arc<SessionWriter>> {
        let live = self.library.open(id).ok()?;
        live.writer.is_writer().then_some(live.writer)
    }
}
