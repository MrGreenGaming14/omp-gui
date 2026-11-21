//! The Updates screen: what each watched product runs and what its feed
//! offers, staged versions, the gate report of the last check with its
//! choices (update anyway, pin, wait), swap between sessions, rollback,
//! app signatures, and the peers that are behind.
//!
//! The screen holds no updater logic. The app fills [`Data`] from its
//! updater service through [`State::set_data`] (see [`Data::from_updater`]),
//! runs the [`Event`]s the screen emits, and calls [`State::finish`] when a
//! step is done.

mod convert;
mod model;
mod style;

pub use model::{Active, Check, Choice, Held, Installed, Kind, Peer, Product, Report, Signature, Standing};

use iced::widget::{Column, button, column, container, row, scrollable, space, text};
use iced::{Element, Fill, Task};
use loa_ui_icons::Icon;
use std::collections::BTreeMap;
use style::{BODY, NOTE, SMALL, Tone};

/// Detail lines shown per gate check before the rest is folded.
const DETAIL_LINES: usize = 12;
/// Release note lines shown before the rest is folded.
const NOTE_LINES: usize = 16;

/// App-supplied snapshot shown by the screen.
#[derive(Clone, Debug, Default)]
pub struct Data {
    pub products: Vec<Product>,
    /// GitHub API requests left in the current window, when known.
    pub github_requests_left: Option<u64>,
    pub peers: Vec<Peer>,
    /// Whether any session is mid turn: swaps wait for it.
    pub turn_running: bool,
    /// Current Unix time, for "checked 5 min ago".
    pub now_unix: u64,
}

/// A step the app is running for a product.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Checking,
    Downloading,
    RunningChecks,
    Verifying,
    Swapping,
}

impl Step {
    fn label(self) -> &'static str {
        match self {
            Self::Checking => "Checking feed…",
            Self::Downloading => "Downloading…",
            Self::RunningChecks => "Running gate checks…",
            Self::Verifying => "Checking signature…",
            Self::Swapping => "Swapping…",
        }
    }
}

/// A request for the app's updater service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    CheckAll,
    Check(String),
    /// Download and unpack the offered release beside the version in use.
    Stage(String),
    /// Run the gate checks of the staged offer.
    Gate(String),
    /// Check the staged app release's signature.
    Verify(String),
    Choose(String, Choice),
    /// Swap in the pending version (between sessions only).
    Swap(String),
    Rollback(String),
    /// Drop the pin and wait of a product.
    Unpin(String),
}

/// Input from the widgets; the app handles the resulting [`Event`].
#[derive(Clone, Debug)]
pub enum Message {
    CheckAll,
    Check(String),
    Stage(String),
    Gate(String),
    Verify(String),
    Choose(String, Choice),
    Swap(String),
    Rollback(String),
    Unpin(String),
    /// Show or hide a product's details.
    Toggle(String),
}

/// The screen's local state.
#[derive(Debug, Default)]
pub struct State {
    pub data: Data,
    /// The product whose details are open.
    pub selected: Option<String>,
    busy: BTreeMap<String, Step>,
}

impl State {
    pub fn new(data: Data) -> Self {
        Self { data, ..Self::default() }
    }

    /// Replaces the snapshot, keeping the open product and running steps.
    pub fn set_data(&mut self, data: Data) {
        if self.selected.as_ref().is_some_and(|id| !data.products.iter().any(|p| &p.id == id)) {
            self.selected = None;
        }
        self.data = data;
    }

    /// The step running for `id`, if any.
    pub fn busy(&self, id: &str) -> Option<Step> {
        self.busy.get(id).copied()
    }

    /// Marks `id`'s step as done.
    pub fn finish(&mut self, id: &str) {
        self.busy.remove(id);
    }

    /// Marks every step as done (after a check of all products).
    pub fn finish_all(&mut self) {
        self.busy.clear();
    }

    fn start(&mut self, id: &str, step: Step) -> bool {
        if self.busy.contains_key(id) {
            return false;
        }
        self.busy.insert(id.to_owned(), step);
        true
    }

    /// Applies an interaction and returns the request for the app, if any.
    /// A product with a step running takes no further requests.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::Toggle(id) => {
                self.selected = if self.selected.as_ref() == Some(&id) { None } else { Some(id) };
                None
            }
            Message::CheckAll => {
                let idle: Vec<String> = self
                    .data
                    .products
                    .iter()
                    .map(|p| p.id.clone())
                    .filter(|id| !self.busy.contains_key(id))
                    .collect();
                if idle.is_empty() {
                    None
                } else {
                    for id in idle {
                        self.busy.insert(id, Step::Checking);
                    }
                    Some(Event::CheckAll)
                }
            }
            Message::Check(id) => self.start(&id, Step::Checking).then_some(Event::Check(id)),
            Message::Stage(id) => self.start(&id, Step::Downloading).then_some(Event::Stage(id)),
            Message::Gate(id) => self.start(&id, Step::RunningChecks).then_some(Event::Gate(id)),
            Message::Verify(id) => self.start(&id, Step::Verifying).then_some(Event::Verify(id)),
            Message::Swap(id) => self.start(&id, Step::Swapping).then_some(Event::Swap(id)),
            Message::Rollback(id) => self.start(&id, Step::Swapping).then_some(Event::Rollback(id)),
            // Instant state edits: no step to track.
            Message::Choose(id, choice) => (!self.busy.contains_key(&id)).then_some(Event::Choose(id, choice)),
            Message::Unpin(id) => (!self.busy.contains_key(&id)).then_some(Event::Unpin(id)),
        };
        (Task::none(), event)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let any_idle = self.data.products.iter().any(|p| !self.busy.contains_key(&p.id));
        let header = row![
            text("Updates").size(BODY),
            space::horizontal(),
            style::flat_button("Check all").on_press_maybe(any_idle.then_some(Message::CheckAll)),
        ]
        .spacing(8)
        .align_y(iced::Center);
        let intro = text(
            "Updates install side by side, pass the gate checks, and swap in between sessions. \
             The previous version stays for rollback.",
        )
        .size(NOTE)
        .style(style::muted);

        let products = self
            .data
            .products
            .iter()
            .fold(column![].spacing(8), |list, product| list.push(self.view_product(product)));
        let mut content = column![header, intro, section("Products", products.into())].spacing(16).padding(12);
        if let Some(left) = self.data.github_requests_left {
            content = content.push(text(format!("GitHub API: {left} requests left this hour")).size(SMALL).style(style::muted));
        }
        content = content.push(section("Peers", self.view_peers()));
        scrollable(content).width(Fill).height(Fill).direction(style::thin_scrollbar()).style(style::scrollbar).into()
    }

    fn view_product<'a>(&'a self, p: &'a Product) -> Element<'a, Message> {
        let open = self.selected.as_deref() == Some(p.id.as_str());
        let (tone, status) = summary(p, self.data.turn_running);
        let chevron = if open { Icon::ChevronDown } else { Icon::ChevronRight };
        let name = button(
            row![
                loa_ui_icons::muted(chevron, 12.0),
                text(p.label.as_str()).size(BODY),
                container(text(kind_label(p.kind)).size(SMALL)).padding([1.0, 6.0]).style(style::badge),
            ]
            .spacing(8)
            .align_y(iced::Center),
        )
        .padding([2.0, 4.0])
        .style(style::flat)
        .on_press(Message::Toggle(p.id.clone()));

        let versions = match (&p.current, &p.offer) {
            (Some(current), Some(offer)) => format!("{current} → {offer}"),
            (Some(current), None) => current.clone(),
            (None, _) => "not installed".to_owned(),
        };
        let status_line = row![
            container(space::horizontal().width(6).height(6)).style(style::dot(tone)),
            text(status).size(NOTE).style(style::toned(tone)),
        ]
        .spacing(6)
        .align_y(iced::Center);

        let top = row![
            name,
            text(versions).size(NOTE).font(iced::Font::MONOSPACE),
            space::horizontal(),
            self.view_actions(p),
        ]
        .spacing(12)
        .align_y(iced::Center);
        let mut body = column![top, status_line].spacing(6);
        if let Some(error) = &p.error {
            body = body.push(container(text(error.as_str()).size(SMALL)).style(style::error_box).padding(8).width(Fill));
        }
        if open {
            body = body.push(style::divider()).push(self.view_details(p));
        }
        container(body).style(style::card).padding(10).width(Fill).into()
    }

    fn view_actions<'a>(&'a self, p: &'a Product) -> Element<'a, Message> {
        if let Some(step) = self.busy(&p.id) {
            return text(step.label()).size(SMALL).style(style::muted).into();
        }
        let id = || p.id.clone();
        let mut actions = row![].spacing(4).align_y(iced::Center);
        if let Some(held) = &p.held {
            actions = actions.push(
                style::danger_button(format!("Update anyway to {}", held.version))
                    .on_press(Message::Choose(id(), Choice::UpdateAnyway)),
            );
            if let Some(current) = &p.current {
                actions = actions
                    .push(style::flat_button(format!("Pin {current}")).on_press(Message::Choose(id(), Choice::PinCurrent)));
            }
            actions = actions.push(style::flat_button("Wait").on_press(Message::Choose(id(), Choice::Wait)));
        } else if let Some(pending) = &p.pending_swap {
            actions = actions.push(style::filled_button(format!("Swap to {pending}")).on_press(Message::Swap(id())));
        } else if let Some(offer) = p.offer.as_ref().filter(|_| p.current.is_some()) {
            if !p.staged.contains(offer) {
                actions = actions.push(style::filled_button(format!("Download {offer}")).on_press(Message::Stage(id())));
            } else if p.report.as_ref().is_none_or(|r| r.to != *offer) {
                actions = actions.push(style::filled_button("Run gate checks").on_press(Message::Gate(id())));
            }
        }
        if p.kind == Kind::App
            && let Some(offer) = &p.offer
            && p.staged.contains(offer)
        {
            actions = actions.push(style::flat_button("Verify signature").on_press(Message::Verify(id())));
        }
        if let Some(previous) = p.active.as_ref().and_then(|a| a.previous.as_ref()) {
            actions = actions.push(style::flat_button(format!("Roll back to {previous}")).on_press(Message::Rollback(id())));
        }
        if p.pin.is_some() || p.waiting_past.is_some() {
            actions = actions.push(style::flat_button("Unpin").on_press(Message::Unpin(id())));
        }
        actions = actions.push(style::flat_button("Check").on_press(Message::Check(id())));
        actions.into()
    }

    fn view_details<'a>(&'a self, p: &'a Product) -> Element<'a, Message> {
        let mut facts: Vec<(&'static str, String)> = Vec::new();
        let checked = match p.last_check_unix {
            Some(at) => format!("checked {}", loa_util::time::ago(self.data.now_unix as i64 * 1000, at as i64 * 1000)),
            None => "not checked yet".to_owned(),
        };
        facts.push(("Feed", format!("{} · {checked}", p.source)));
        if let Some(newest) = &p.newest {
            facts.push(("Newest release", newest.clone()));
        }
        facts.push((
            "In use",
            match &p.installed {
                Installed::Unknown => "not looked for yet".to_owned(),
                Installed::Running => "this app".to_owned(),
                Installed::Managed(path) => format!("{path} (installed by the updater)"),
                Installed::System(path) => path.clone(),
                Installed::Missing(reason) => format!("not installed: {reason}"),
            },
        ));
        if let Some(active) = &p.active {
            let previous = active.previous.as_deref().unwrap_or("none");
            facts.push(("Active", format!("{} (rollback to {previous})", active.current)));
        }
        facts.push(("Staged", if p.staged.is_empty() { "none".to_owned() } else { p.staged.join(", ") }));
        if let Some(pin) = &p.pin {
            facts.push(("Pinned", pin.clone()));
        }
        if let Some(waited) = &p.waiting_past {
            facts.push(("Waiting", format!("skipping {waited} and older")));
        }
        if p.kind == Kind::App {
            facts.push(("Signature", signature_label(p.signature.as_ref()).to_owned()));
        }
        let facts = facts.into_iter().fold(column![].spacing(4), |list, (label, value)| {
            list.push(
                row![text(label).size(SMALL).style(style::muted).width(110), text(value).size(SMALL).width(Fill)]
                    .spacing(8),
            )
        });

        let mut details = column![facts].spacing(12);
        match &p.report {
            Some(report) => details = details.push(view_report(report)),
            None => details = details.push(text("No gate report yet.").size(SMALL).style(style::muted)),
        }
        if !p.changelog.trim().is_empty() {
            details = details.push(view_notes(&p.changelog));
        }
        details.into()
    }

    fn view_peers(&self) -> Element<'_, Message> {
        if self.data.peers.is_empty() {
            return text("No peers yet.").size(NOTE).style(style::muted).into();
        }
        let behind = self.data.peers.iter().filter(|p| matches!(p.standing, Standing::Behind { .. })).count();
        let summary = match behind {
            0 => "No peer is behind.".to_owned(),
            1 => "1 peer is behind.".to_owned(),
            n => format!("{n} peers are behind."),
        };
        let rows = self.data.peers.iter().fold(column![].spacing(6), |list, peer| {
            let (tone, standing) = standing_label(peer);
            let name = peer.name.clone().unwrap_or_else(|| short_node(&peer.node));
            let shared: Vec<String> = peer
                .protocols
                .iter()
                .map(|(family, shared)| match shared {
                    Some(v) => format!("{family} {v}"),
                    None => format!("{family} none"),
                })
                .collect();
            list.push(
                row![
                    container(space::horizontal().width(6).height(6)).style(style::dot(tone)),
                    text(name).size(NOTE).width(180),
                    text(peer.version.as_deref().unwrap_or("unknown")).size(NOTE).font(iced::Font::MONOSPACE).width(90),
                    text(standing).size(NOTE).style(style::toned(tone)).width(220),
                    text(shared.join(" · ")).size(SMALL).style(style::muted).width(Fill),
                ]
                .spacing(8)
                .align_y(iced::Center),
            )
        });
        column![text(summary).size(NOTE).style(style::muted), rows].spacing(8).into()
    }
}

fn section<'a>(title: &'a str, body: Element<'a, Message>) -> Element<'a, Message> {
    column![style::section_header(title), style::divider(), body].spacing(8).into()
}

fn view_report(report: &Report) -> Element<'_, Message> {
    let harmful = report.harmful();
    let (tone, verdict) = if report.checks.is_empty() {
        (Tone::Good, "No gate checks apply; the signature is checked at swap.".to_owned())
    } else if harmful == 0 {
        (Tone::Good, format!("All {} checks passed.", report.checks.len()))
    } else {
        (Tone::Bad, format!("{harmful} of {} checks found something harmful.", report.checks.len()))
    };
    let head = row![
        text(format!("Gate report {} → {}", report.from, report.to)).size(NOTE),
        text(verdict).size(NOTE).style(style::toned(tone)),
    ]
    .spacing(12);
    let checks = report.checks.iter().fold(column![].spacing(8), |list, check| {
        let (icon_name, tone, verdict) = if check.pass {
            (Icon::Check, Tone::Good, "pass")
        } else {
            (Icon::TriangleAlert, Tone::Bad, "harmful")
        };
        let label = row![icon(icon_name, tone), text(check.label.as_str()).size(SMALL), text(verdict).size(SMALL).style(style::toned(tone))]
            .spacing(6)
            .align_y(iced::Center);
        let lines = check.details.iter().take(DETAIL_LINES).fold(Column::new().spacing(2), |lines, line| {
            lines.push(text(line.as_str()).size(SMALL).font(iced::Font::MONOSPACE).style(style::muted))
        });
        let mut entry = column![label].spacing(4);
        if !check.details.is_empty() {
            entry = entry.push(container(lines).padding([0.0, 18.0]));
        }
        if check.details.len() > DETAIL_LINES {
            entry = entry.push(
                container(text(format!("… {} more", check.details.len() - DETAIL_LINES)).size(SMALL).style(style::muted))
                    .padding([0.0, 18.0]),
            );
        }
        list.push(entry)
    });
    column![head, checks].spacing(8).into()
}

fn view_notes(notes: &str) -> Element<'_, Message> {
    let total = notes.lines().count();
    let body = notes
        .lines()
        .take(NOTE_LINES)
        .fold(Column::new().spacing(2), |col, line| col.push(text(line).size(SMALL).style(style::muted)));
    let mut notes_col = column![text("Release notes").size(NOTE), body].spacing(6);
    if total > NOTE_LINES {
        notes_col = notes_col.push(text(format!("… {} more lines", total - NOTE_LINES)).size(SMALL).style(style::muted));
    }
    notes_col.into()
}

fn icon<'a>(name: Icon, tone: Tone) -> Element<'a, Message> {
    loa_ui_icons::icon(name, 12.0)
        .style(move |theme: &iced::Theme, _| iced::widget::svg::Style { color: Some(style::tone_color(theme, tone)) })
        .into()
}

/// One status line for a product.
fn summary(p: &Product, turn_running: bool) -> (Tone, String) {
    if p.restart_required {
        let installed = p.active.as_ref().map_or_else(String::new, |a| format!(" {}", a.current));
        return (Tone::Attention, format!("Installed{installed}; restart the app to use it"));
    }
    if let Some(held) = &p.held {
        let checks = if held.harmful == 1 { "check" } else { "checks" };
        return (Tone::Bad, format!("{} held: {} harmful {checks}. Update anyway, pin or wait.", held.version, held.harmful));
    }
    if let Some(pending) = &p.pending_swap {
        return if turn_running {
            (Tone::Good, format!("Checks passed; {pending} swaps in when the running turn ends"))
        } else {
            (Tone::Good, format!("Checks passed; ready to swap to {pending}"))
        };
    }
    if p.current.is_none() {
        return match (&p.installed, &p.newest) {
            (Installed::Unknown, _) => (Tone::Neutral, "Looking for the installed version…".to_owned()),
            (_, Some(newest)) => (Tone::Neutral, format!("Not installed; latest is {newest}")),
            (_, None) => (Tone::Neutral, "Not installed".to_owned()),
        };
    }
    if let Some(offer) = &p.offer {
        return if p.staged.contains(offer) {
            (Tone::Attention, format!("{offer} downloaded; run the gate checks"))
        } else {
            (Tone::Attention, format!("Update available: {offer}"))
        };
    }
    if let Some(pin) = &p.pin {
        return (Tone::Neutral, format!("Pinned to {pin}"));
    }
    if let Some(waited) = &p.waiting_past {
        return (Tone::Neutral, format!("Waiting for a version after {waited}"));
    }
    if p.last_check_unix.is_none() {
        return (Tone::Neutral, "Not checked yet".to_owned());
    }
    (Tone::Good, "Up to date".to_owned())
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::App => "app",
        Kind::Omp => "omp",
        Kind::Harness => "harness",
    }
}

fn signature_label(status: Option<&Signature>) -> &str {
    match status {
        None => "not checked (download the update first)",
        Some(Signature::Verified) => "verified with a trusted key",
        Some(Signature::Unsigned) => "unsigned: it will not be swapped in",
        Some(Signature::NoTrustedKeys) => "no trusted release key configured: updates cannot be verified",
        Some(Signature::Rejected(reason)) => reason,
    }
}

fn standing_label(peer: &Peer) -> (Tone, String) {
    if !peer.can_talk {
        return (Tone::Bad, "can't talk: no shared protocol".to_owned());
    }
    match peer.standing {
        Standing::Current => (Tone::Good, "current".to_owned()),
        Standing::Ahead => (Tone::Neutral, "ahead of this app".to_owned()),
        Standing::Unknown => (Tone::Neutral, "version unknown".to_owned()),
        Standing::Behind { majors, minors, patches } => {
            let (n, one, many) = if majors > 0 {
                (majors, "major", "majors")
            } else if minors > 0 {
                (minors, "minor", "minors")
            } else {
                (patches, "patch", "patches")
            };
            match n {
                0 => (Tone::Attention, "behind (prerelease)".to_owned()),
                1 => (Tone::Attention, format!("behind by 1 {one}")),
                n => (Tone::Attention, format!("behind by {n} {many}")),
            }
        }
    }
}

fn short_node(node: &str) -> String {
    match node.char_indices().nth(12) {
        Some((at, _)) => format!("{}…", &node[..at]),
        None => node.to_owned(),
    }
}

#[cfg(test)]
mod tests;
