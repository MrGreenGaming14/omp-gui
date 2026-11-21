//! The Session tree tab's data: the fork family of the active session, built
//! from the `parent_session` / `forked_at` links on each session's root and
//! the turn rows of the journal. Journal reads run off the UI thread; the tab
//! shows what it knows and refreshes when a read lands.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use iced::Task;
use loa_bus::Listing;
use loa_core::{Dom, ElementKind, NodeId, Role, attr};
use loa_session::library::Library;
use loa_ui_panel::session_tree;
use loa_view::panel::{SessionInfo, TurnInfo};

use super::Message;
use crate::area::Ctx;

/// Where a session branched off.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Link {
    parent: Option<String>,
    forked_at: Option<u64>,
}

impl Link {
    fn of(dom: &Dom) -> Self {
        let root = dom.root();
        Self {
            parent: root.and_then(|root| root.str_attr(attr::PARENT_SESSION)).map(str::to_owned),
            forked_at: root.and_then(|root| root.int_attr(attr::FORKED_AT)).and_then(|seq| u64::try_from(seq).ok()),
        }
    }
}

/// Turn rows of one session, as read from its journal.
#[derive(Clone, Debug, Default)]
struct Turns {
    /// The journal sequence that started each turn, in journal order.
    seqs: Vec<(NodeId, u64)>,
    /// First line of each turn; kept only for a session closed at read time.
    lines: Option<HashMap<NodeId, String>>,
}

/// What one background read of the journals found.
#[derive(Clone, Debug, Default)]
pub struct Loaded {
    scanned: bool,
    links: HashMap<String, Link>,
    turns: HashMap<String, Turns>,
}

#[derive(Default)]
pub struct Tree {
    pub view: session_tree::State,
    /// Links of every stored session; read once, then kept up to date.
    links: HashMap<String, Link>,
    scanned: bool,
    /// Turn rows per session, in journal order; kept up to date by `record_turn`.
    seqs: HashMap<String, Vec<(NodeId, u64)>>,
    /// First lines of sessions that were closed when their turns were read.
    closed_lines: HashMap<String, HashMap<NodeId, String>>,
    /// Turns recorded while a read was in flight; merged into its result.
    pending: HashMap<String, Vec<(NodeId, u64)>>,
    loading: bool,
}

impl Tree {
    /// Drops a deleted session.
    pub fn forget(&mut self, id: &str) {
        self.links.remove(id);
        self.seqs.remove(id);
        self.closed_lines.remove(id);
        self.pending.remove(id);
    }

    /// Notes a turn that session `id` just gained, so the tab shows it before the next journal
    /// read.
    pub fn record_turn(&mut self, id: &str, turn: NodeId, seq: u64) {
        self.closed_lines.remove(id);
        if let Some(seqs) = self.seqs.get_mut(id)
            && !seqs.iter().any(|(node, _)| *node == turn)
        {
            seqs.push((turn, seq));
        }
        if self.loading {
            self.pending.entry(id.to_owned()).or_default().push((turn, seq));
        }
    }

    /// Rebuilds the tab for the active session's family. Turns the tab lacks are read off the
    /// UI thread; the result arrives as `Message::TreeLoaded`.
    pub fn refresh(&mut self, ctx: &Ctx) -> Task<Message> {
        let Some(active) = ctx.runtime.sessions().active() else {
            self.view.set_family(Vec::new(), "");
            return Task::none();
        };
        let current = active.id().to_string();
        self.links.insert(current.clone(), Link::of(active.dom()));
        for session in ctx.runtime.sessions().iter() {
            self.links.insert(session.id().to_string(), Link::of(session.dom()));
        }

        // The family's root: the oldest ancestor still stored.
        let mut top = current.clone();
        let mut seen = HashSet::from([top.clone()]);
        while let Some(parent) = self.links.get(&top).and_then(|link| link.parent.clone()) {
            if !self.links.contains_key(&parent) || !seen.insert(parent.clone()) {
                break;
            }
            top = parent;
        }
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for (id, link) in &self.links {
            if let Some(parent) = &link.parent {
                children.entry(parent.as_str()).or_default().push(id.as_str());
            }
        }
        let mut family = Vec::new();
        let mut queue = vec![top.as_str()];
        let mut visited = HashSet::new();
        while let Some(id) = queue.pop() {
            if !visited.insert(id) {
                continue;
            }
            family.push(id);
            if let Some(kids) = children.get(id) {
                queue.extend(kids.iter().copied());
            }
        }
        let mut missing = Vec::new();
        let infos = family
            .iter()
            .map(|id| {
                let link = self.links.get(*id).cloned().unwrap_or_default();
                let turns = if *id == current || children.contains_key(id) {
                    if self.needs_read(ctx, id) {
                        missing.push((*id).to_owned());
                    }
                    self.turns_of(ctx, id)
                } else {
                    Vec::new()
                };
                let title = ctx.shell.sidebar().session(id).map(|summary| summary.title.clone()).unwrap_or_default();
                SessionInfo { id: (*id).to_owned(), parent: link.parent, forked_at: link.forked_at, title, turns }
            })
            .collect();
        self.view.set_family(infos, &current);

        if (!self.scanned || !missing.is_empty()) && !self.loading {
            self.loading = true;
            let library = Arc::clone(ctx.runtime.library());
            let scan = !self.scanned;
            let open: HashSet<String> = ctx.runtime.sessions().iter().map(|session| session.id().to_string()).collect();
            return Task::perform(
                async move { tokio::task::spawn_blocking(move || read(&library, scan, &missing, &open)).await.expect("the session tree read panicked") },
                |loaded| Message::TreeLoaded(Box::new(loaded)),
            );
        }
        Task::none()
    }

    /// Applies a background read. Turns recorded while it ran are kept.
    pub fn merge(&mut self, loaded: Loaded) {
        self.loading = false;
        if loaded.scanned {
            self.scanned = true;
            for (id, link) in loaded.links {
                self.links.entry(id).or_insert(link);
            }
        }
        let mut pending = std::mem::take(&mut self.pending);
        for (id, turns) in loaded.turns {
            if !self.links.contains_key(&id) {
                continue;
            }
            let Turns { mut seqs, mut lines } = turns;
            if let Some(recorded) = pending.remove(&id) {
                let mut known = HashSet::new();
                seqs.extend(recorded);
                seqs.retain(|(node, _)| known.insert(*node));
                seqs.sort_by_key(|(_, seq)| *seq);
                // The lines were read before these turns existed; a later read repairs them.
                lines = None;
            }
            match lines {
                Some(lines) => {
                    self.closed_lines.insert(id.clone(), lines);
                }
                None => {
                    self.closed_lines.remove(&id);
                }
            }
            self.seqs.insert(id, seqs);
        }
    }

    /// Whether the tab lacks the turn rows of session `id`.
    fn needs_read(&self, ctx: &Ctx, id: &str) -> bool {
        !self.seqs.contains_key(id) || (ctx.runtime.sessions().get(id).is_none() && !self.closed_lines.contains_key(id))
    }

    /// The turn rows of session `id`. First lines come from the open DOM, or from the cached
    /// lines of a closed session.
    fn turns_of(&self, ctx: &Ctx, id: &str) -> Vec<TurnInfo> {
        let seqs = self.seqs.get(id).map(Vec::as_slice).unwrap_or_default();
        match ctx.runtime.sessions().get(id) {
            Some(session) => {
                let dom = session.dom();
                seqs.iter().filter(|(node, _)| dom.contains(*node)).map(|&(node, seq)| TurnInfo { node, seq, first_line: first_line(dom, node) }).collect()
            }
            None => {
                let lines = self.closed_lines.get(id);
                seqs.iter().map(|&(node, seq)| TurnInfo { node, seq, first_line: lines.and_then(|lines| lines.get(&node)).cloned().unwrap_or_default() }).collect()
            }
        }
    }
}

/// Journal reads for [`Tree::refresh`]: the links of every stored session when `scan` is set,
/// and the turn rows of `missing`. Sessions in `open` are read from memory by the caller, so
/// their links and lines are skipped here.
fn read(library: &Library, scan: bool, missing: &[String], open: &HashSet<String>) -> Loaded {
    let mut loaded = Loaded { scanned: scan, ..Loaded::default() };
    if scan {
        for listing in library.store().list().unwrap_or_default() {
            let Listing::Session(info) = listing else { continue };
            if open.contains(&info.id) {
                continue;
            }
            if let Ok(dom) = library.load(&info.id) {
                loaded.links.insert(info.id, Link::of(&dom));
            }
        }
    }
    for id in missing {
        let seqs = library.turn_seqs(id).unwrap_or_default();
        let lines = (!open.contains(id)).then(|| {
            let dom = library.load(id).ok();
            seqs.iter().filter_map(|&(node, _)| Some((node, first_line(dom.as_ref()?, node)))).collect()
        });
        loaded.turns.insert(id.clone(), Turns { seqs, lines });
    }
    loaded
}

/// First line of the user's prompt in `turn`.
fn first_line(dom: &Dom, turn: NodeId) -> String {
    dom.children_of_kind(turn, ElementKind::Message)
        .find(|message| message.str_attr(attr::ROLE) == Some(Role::User.as_str()))
        .and_then(|message| message.text.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or_default()
        .to_owned()
}
