//! The Session tree tab's data: the fork family of the active session, built
//! from the `parent_session` / `forked_at` links on each session's root and
//! the turn rows of the journal.

use std::collections::{HashMap, HashSet};

use loa_core::{Dom, ElementKind, NodeId, Role, attr};
use loa_bus::Listing;
use loa_ui_panel::session_tree::{self, SessionInfo, TurnInfo};

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

#[derive(Default)]
pub struct Tree {
    pub view: session_tree::State,
    /// Links of every stored session; read once, then kept up to date.
    links: HashMap<String, Link>,
    scanned: bool,
}

impl Tree {
    /// Drops a deleted session.
    pub fn forget(&mut self, id: &str) {
        self.links.remove(id);
    }

    /// Rebuilds the tab for the active session's family.
    pub fn refresh(&mut self, ctx: &Ctx) {
        let Some(active) = ctx.sessions.active() else {
            self.view.set_family(Vec::new(), "");
            return;
        };
        let current = active.id().to_string();
        self.links.insert(current.clone(), Link::of(active.dom()));
        if !self.scanned {
            self.scan(ctx);
        }
        for session in ctx.sessions.iter() {
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
        let infos = family
            .iter()
            .map(|id| {
                let link = self.links.get(*id).cloned().unwrap_or_default();
                let turns = if *id == current || children.contains_key(id) { turns(ctx, id) } else { Vec::new() };
                let title = ctx.shell.sidebar().session(id).map(|summary| summary.title.clone()).unwrap_or_default();
                SessionInfo { id: (*id).to_owned(), parent: link.parent, forked_at: link.forked_at, title, turns }
            })
            .collect();
        self.view.set_family(infos, &current);
    }

    /// Reads the links of every stored session.
    fn scan(&mut self, ctx: &Ctx) {
        let library = &ctx.services.storage.library;
        if let Ok(listings) = library.store().list() {
            for listing in listings {
                let Listing::Session(info) = listing else { continue };
                if self.links.contains_key(&info.id) {
                    continue;
                }
                if let Ok(dom) = library.load(&info.id) {
                    self.links.insert(info.id, Link::of(&dom));
                }
            }
        }
        self.scanned = true;
    }
}

/// The turns of session `id` with the journal sequence that started each.
fn turns(ctx: &Ctx, id: &str) -> Vec<TurnInfo> {
    let library = &ctx.services.storage.library;
    let Ok(seqs) = library.turn_seqs(id) else { return Vec::new() };
    let loaded;
    let dom = match ctx.sessions.get(id) {
        Some(session) => session.dom(),
        None => match library.load(id) {
            Ok(dom) => {
                loaded = dom;
                &loaded
            }
            Err(_) => return Vec::new(),
        },
    };
    seqs.into_iter().map(|(node, seq)| TurnInfo { node, seq, first_line: first_line(dom, node) }).collect()
}

/// First line of the user's prompt in `turn`.
fn first_line(dom: &Dom, turn: NodeId) -> String {
    dom.children_of_kind(turn, ElementKind::Message)
        .find(|message| message.str_attr(attr::ROLE) == Some(Role::User.as_str()))
        .and_then(|message| message.text.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or_default()
        .to_owned()
}
