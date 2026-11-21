//! Updates: what each watched product runs and what its feed offers, staged versions, the gate
//! report of the last check with its choices (update anyway, pin, wait), swaps between sessions,
//! rollback, app signatures and the peers that are behind, drawn straight from the updater's
//! status. The app runs the [`Event`]s and calls [`State::finish`] when a step is done.

use crate::{action, dot, labeled, scroll, section};
use iced::widget::{Column, Space, button, column, container, row};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::Icon;
use loa_ui_kit::icons::tinted;
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole, TextRole, sp};
use loa_ui_kit::widgets::{Msg, Screen, badge, label};
use loa_settings::updates::{
    GateReport, HoldChoice, InstalledStatus, PeerStanding, ProductKind, ProductStatus, SignatureStatus, Standing, Verdict,
};
use std::collections::BTreeMap;

/// Detail lines shown per gate check before the rest is folded.
const DETAIL_LINES: usize = 12;
/// Release note lines shown before the rest is folded.
const NOTE_LINES: usize = 16;

/// A status line in its tone when the rule holds for a product, given whether a turn runs.
type Rule = (Tone, fn(&ProductStatus, bool) -> Option<String>);

/// A product's status line: the first rule with a line wins.
const STATUS_RULES: &[Rule] = &[
    (Tone::Warn, |p, _| p.restart_required.then(|| format!("Installed{}; restart the app to use it", p.active.as_ref().map_or_else(String::new, |a| format!(" {}", a.current))))),
    (Tone::Danger, |p, _| p.held.as_ref().map(|h| match h.report.harmful().count() { 1 => format!("{} held: 1 harmful check. Update anyway, pin or wait.", h.version), n => format!("{} held: {n} harmful checks. Update anyway, pin or wait.", h.version) })),
    (Tone::Ok, |p, running| p.pending_swap.as_ref().filter(|_| running).map(|v| format!("Checks passed; {v} swaps in when the running turn ends"))),
    (Tone::Ok, |p, _| p.pending_swap.as_ref().map(|v| format!("Checks passed; ready to swap to {v}"))),
    (Tone::Muted, |p, _| (p.current.is_none() && p.installed == InstalledStatus::Unknown).then(|| "Looking for the installed version…".to_owned())),
    (Tone::Muted, |p, _| p.current.is_none().then(|| p.newest.as_ref().map_or_else(|| "Not installed".to_owned(), |n| format!("Not installed; latest is {n}")))),
    (Tone::Warn, |p, _| p.offer.as_ref().filter(|o| p.staged.contains(&o.version)).map(|o| format!("{} downloaded; run the gate checks", o.version))),
    (Tone::Warn, |p, _| p.offer.as_ref().map(|o| format!("Update available: {}", o.version))),
    (Tone::Muted, |p, _| p.pin.as_ref().map(|v| format!("Pinned to {v}"))),
    (Tone::Muted, |p, _| p.waiting_past.as_ref().map(|v| format!("Waiting for a version after {v}"))),
    (Tone::Muted, |p, _| p.last_check_unix.is_none().then(|| "Not checked yet".to_owned())),
    (Tone::Ok, |_, _| Some("Up to date".to_owned())),
];

/// The status line of `p` and its tone.
pub fn status(p: &ProductStatus, turn_running: bool) -> (Tone, String) {
    STATUS_RULES.iter().find_map(|(tone, rule)| rule(p, turn_running).map(|line| (*tone, line))).expect("the last rule always matches")
}

/// The app's snapshot the screen shows.
#[derive(Clone, Debug, Default)]
pub struct Data {
    pub products: Vec<ProductStatus>,
    /// GitHub API requests left in the current window, when known.
    pub github_requests_left: Option<u64>,
    pub peers: Vec<PeerStanding>,
    /// Whether a session is mid turn: swaps wait for it.
    pub turn_running: bool,
    /// Current Unix time, for "checked 5m ago".
    pub now_unix: u64,
}

loa_util::str_enum! {
    /// A step the app runs for a product, named by its busy label.
    pub Step { Checking => "Checking feed…", Downloading => "Downloading…", RunningChecks => "Running gate checks…", Verifying => "Checking signature…", Swapping => "Swapping…" }
}

/// A request for the app's updater.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    CheckAll,
    Check(String),
    /// Download and unpack the offer beside the version in use.
    Stage(String),
    /// Run the staged offer's gate checks.
    Gate(String),
    /// Check the staged app release's signature.
    Verify(String),
    /// The answer to a held update.
    Choose(String, HoldChoice),
    /// Swap in the pending version (between turns only).
    Swap(String),
    Rollback(String),
    /// Drop a product's pin and wait.
    Unpin(String),
}

impl Event {
    /// The product and the step the event runs; choices and unpins are instant.
    fn step(&self) -> Option<(&str, Step)> {
        Some(match self {
            Self::Check(id) => (id, Step::Checking),
            Self::Stage(id) => (id, Step::Downloading),
            Self::Gate(id) => (id, Step::RunningChecks),
            Self::Verify(id) => (id, Step::Verifying),
            Self::Swap(id) | Self::Rollback(id) => (id, Step::Swapping),
            Self::CheckAll | Self::Choose(..) | Self::Unpin(_) => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub enum Local {
    /// Shows or hides a product's details.
    Toggle(String),
    /// Starts a step (or every check), marking its products busy.
    Start(Event),
}

pub type Message = Msg<Local, Event>;

#[derive(Debug, Default)]
pub struct State {
    pub data: Data,
    /// The product whose details are open.
    pub selected: Option<String>,
    busy: BTreeMap<String, Step>,
}

impl State {
    pub fn new(data: Data) -> Self { Self { data, ..Self::default() } }

    /// Replaces the snapshot, keeping the open product and running steps.
    pub fn set_data(&mut self, data: Data) {
        self.selected = self.selected.take().filter(|id| data.products.iter().any(|p| &p.id == id));
        self.data = data;
    }

    pub fn busy(&self, id: &str) -> Option<Step> { self.busy.get(id).copied() }
    /// Marks `id`'s step done.
    pub fn finish(&mut self, id: &str) { self.busy.remove(id); }
    /// Marks every step done (after checking every product).
    pub fn finish_all(&mut self) { self.busy.clear() }

    fn idle(&self) -> impl Iterator<Item = &ProductStatus> { self.data.products.iter().filter(|p| !self.busy.contains_key(&p.id)) }

    pub fn view(&self) -> Element<'_, Message> {
        let check_all = (self.idle().next().is_some()).then_some(Msg::Local(Local::Start(Event::CheckAll)));
        let head = row![label("Updates", TextRole::Body, Tone::Normal), Space::new().width(Fill), action("Check all", ButtonKind::Ghost, check_all)].align_y(Alignment::Center);
        let intro = label("Updates install side by side, pass the gate checks, and swap in between sessions. The previous version stays for rollback.", TextRole::Label, Tone::Muted);
        let products = Column::with_children(self.data.products.iter().map(|p| self.product(p))).spacing(sp(SpaceRole::S));
        let limit = self.data.github_requests_left.map(|left| label(format!("GitHub API: {left} requests left this hour"), TextRole::Caption, Tone::Muted));
        scroll(column![head, intro, section("Products", products), limit, section("Peers", self.peers())].spacing(sp(SpaceRole::L)))
    }

    fn product<'a>(&'a self, p: &'a ProductStatus) -> Element<'a, Message> {
        let open = self.selected.as_deref() == Some(p.id.as_str());
        let (tone, line) = status(p, self.data.turn_running);
        let kind = match p.kind { ProductKind::App => "app", ProductKind::Omp => "omp", ProductKind::Harness(_) => "harness" };
        let name = row![tinted(if open { Icon::ChevronDown } else { Icon::ChevronRight }, TextRole::Caption, Tone::Muted), label(p.label.as_str(), TextRole::Body, Tone::Normal), badge(kind, Tone::Muted)];
        let name = button(name.spacing(sp(SpaceRole::S)).align_y(Alignment::Center)).padding(sp(SpaceRole::Xs)).style(style::button(ButtonKind::Flat)).on_press(Msg::Local(Local::Toggle(p.id.clone())));
        let versions = match (&p.current, &p.offer) {
            (Some(current), Some(offer)) => format!("{current} → {}", offer.version),
            (Some(current), None) => current.to_string(),
            (None, _) => "not installed".to_owned(),
        };
        let top = row![name, label(versions, TextRole::Mono, Tone::Normal), Space::new().width(Fill), self.actions(p)].spacing(sp(SpaceRole::M)).align_y(Alignment::Center);
        let line = row![dot(tone), label(line, TextRole::Label, tone)].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
        let error = p.error.as_ref().map(|e| crate::errors([e.clone()]));
        let details = open.then(|| column![iced::widget::rule::horizontal(1).style(style::rule), self.details(p)].spacing(sp(SpaceRole::S)));
        container(column![top, line, error, details].spacing(sp(SpaceRole::S))).padding(sp(SpaceRole::S)).width(Fill).style(style::container(Surface::Card)).into()
    }

    fn actions<'a>(&'a self, p: &'a ProductStatus) -> Element<'a, Message> {
        if let Some(step) = self.busy(&p.id) {
            return label(step.as_str(), TextRole::Caption, Tone::Muted).into();
        }
        let id = || p.id.clone();
        let start = |e: Event| Some(Msg::Local(Local::Start(e)));
        let choose = |c| Some(Msg::Emit(Event::Choose(id(), c)));
        let mut actions = row![].spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center);
        let staged_offer = p.offer.as_ref().map(|o| &o.version).filter(|v| p.staged.contains(v));
        if let Some(held) = &p.held {
            actions = actions.push(action(format!("Update anyway to {}", held.version), ButtonKind::Danger, choose(HoldChoice::UpdateAnyway)));
            actions = actions.push(p.current.as_ref().map(|c| action(format!("Pin {c}"), ButtonKind::Ghost, choose(HoldChoice::PinCurrent))));
            actions = actions.push(action("Wait", ButtonKind::Ghost, choose(HoldChoice::Wait)));
        } else if let Some(pending) = &p.pending_swap {
            actions = actions.push(action(format!("Swap to {pending}"), ButtonKind::Primary, start(Event::Swap(id()))));
        } else if let Some(offer) = p.offer.as_ref().filter(|_| p.current.is_some()) {
            if staged_offer.is_none() {
                actions = actions.push(action(format!("Download {}", offer.version), ButtonKind::Primary, start(Event::Stage(id()))));
            } else if p.report.as_ref().is_none_or(|r| r.to != offer.version) {
                actions = actions.push(action("Run gate checks", ButtonKind::Primary, start(Event::Gate(id()))));
            }
        }
        actions = actions.push((p.kind == ProductKind::App && staged_offer.is_some()).then(|| action("Verify signature", ButtonKind::Ghost, start(Event::Verify(id())))));
        actions = actions.push(p.active.as_ref().and_then(|a| a.previous.as_ref()).map(|v| action(format!("Roll back to {v}"), ButtonKind::Ghost, start(Event::Rollback(id())))));
        actions = actions.push((p.pin.is_some() || p.waiting_past.is_some()).then(|| action("Unpin", ButtonKind::Ghost, Some(Msg::Emit(Event::Unpin(id()))))));
        actions.push(action("Check", ButtonKind::Ghost, start(Event::Check(id())))).into()
    }

    fn details<'a>(&'a self, p: &'a ProductStatus) -> Element<'a, Message> {
        let checked = p.last_check_unix.map_or_else(|| "not checked yet".to_owned(), |at| format!("checked {}", loa_util::time::ago(self.data.now_unix as i64 * 1000, at as i64 * 1000)));
        let in_use = match &p.installed {
            InstalledStatus::Unknown => "not looked for yet".to_owned(),
            InstalledStatus::Running => "this app".to_owned(),
            InstalledStatus::Managed(path) => format!("{} (installed by the updater)", path.display()),
            InstalledStatus::System(path) => path.display().to_string(),
            InstalledStatus::Missing(reason) => format!("not installed: {reason}"),
        };
        let signature = match &p.signature {
            None => "not checked (download the update first)".to_owned(),
            Some(SignatureStatus::Verified) => "verified with a trusted key".to_owned(),
            Some(SignatureStatus::Unsigned) => "unsigned: it will not be swapped in".to_owned(),
            Some(SignatureStatus::NoTrustedKeys) => "no trusted release key configured: updates cannot be verified".to_owned(),
            Some(SignatureStatus::Rejected(reason)) => reason.clone(),
        };
        let staged = if p.staged.is_empty() { "none".to_owned() } else { p.staged.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ") };
        let facts = [
            Some(("Feed", format!("{} · {checked}", p.source))),
            p.newest.as_ref().map(|n| ("Newest release", n.to_string())),
            Some(("In use", in_use)),
            p.active.as_ref().map(|a| ("Active", format!("{} (rollback to {})", a.current, a.previous.as_ref().map_or_else(|| "none".to_owned(), ToString::to_string)))),
            Some(("Staged", staged)),
            p.pin.as_ref().map(|v| ("Pinned", v.to_string())),
            p.waiting_past.as_ref().map(|v| ("Waiting", format!("skipping {v} and older"))),
            (p.kind == ProductKind::App).then_some(("Signature", signature)),
        ];
        let facts = Column::with_children(facts.into_iter().flatten().map(|(name, value)| labeled(name, label(value, TextRole::Caption, Tone::Normal)))).spacing(sp(SpaceRole::Xs));
        let report = p.report.as_ref().map_or_else(|| label("No gate report yet.", TextRole::Caption, Tone::Muted).into(), report);
        let notes = (!p.changelog.trim().is_empty()).then(|| folded(Some("Release notes"), p.changelog.lines(), NOTE_LINES, " lines"));
        column![facts, report, notes].spacing(sp(SpaceRole::M)).into()
    }

    fn peers(&self) -> Element<'_, Message> {
        let peers = &self.data.peers;
        if peers.is_empty() { return label("No peers yet.", TextRole::Label, Tone::Muted).into() }
        let summary = match peers.iter().filter(|p| matches!(p.standing, Standing::Behind { .. })).count() {
            0 => "No peer is behind.".to_owned(),
            1 => "1 peer is behind.".to_owned(),
            n => format!("{n} peers are behind."),
        };
        let rows = peers.iter().map(|p| {
            let (tone, standing) = standing(p);
            let name = p.name.clone().unwrap_or_else(|| loa_ui_kit::text::truncate_trailing(&p.node, 13));
            let shared = p.protocols.iter().map(|s| s.shared.map_or_else(|| format!("{} none", s.family), |v| format!("{} {v}", s.family))).collect::<Vec<_>>().join(" · ");
            let version = p.app_version.as_ref().map_or_else(|| "unknown".to_owned(), ToString::to_string);
            row![dot(tone), label(name, TextRole::Label, Tone::Normal), label(version, TextRole::Mono, Tone::Normal), label(standing, TextRole::Label, tone), label(shared, TextRole::Caption, Tone::Muted)].spacing(sp(SpaceRole::M)).align_y(Alignment::Center).into()
        });
        column![label(summary, TextRole::Label, Tone::Muted)].extend(rows).spacing(sp(SpaceRole::S)).into()
    }
}

/// An optional heading over the first `shown` of `lines`, then how many more there are.
fn folded<'a>(head: Option<&'a str>, lines: impl Iterator<Item = &'a str> + Clone, shown: usize, unit: &str) -> Element<'a, Message> {
    let total = lines.clone().count();
    let more = (total > shown).then(|| label(format!("… {} more{unit}", total - shown), TextRole::Caption, Tone::Muted));
    let head = head.map(|h| label(h, TextRole::Label, Tone::Normal));
    column![head].extend(lines.take(shown).map(|l| label(l, TextRole::Caption, Tone::Muted).into())).push(more).spacing(sp(SpaceRole::Xs)).into()
}

fn report(r: &GateReport) -> Element<'_, Message> {
    let harmful = r.harmful().count();
    let (tone, verdict) = match (r.checks.len(), harmful) {
        (0, _) => (Tone::Ok, "No gate checks apply; the signature is checked at swap.".to_owned()),
        (n, 0) => (Tone::Ok, format!("All {n} checks passed.")),
        (n, h) => (Tone::Danger, format!("{h} of {n} checks found something harmful.")),
    };
    let head = row![label(format!("Gate report {} → {}", r.from, r.to), TextRole::Label, Tone::Normal), label(verdict, TextRole::Label, tone)].spacing(sp(SpaceRole::M));
    let checks = r.checks.iter().map(|c| {
        let (icon, tone, verdict) = if c.verdict == Verdict::Pass { (Icon::Check, Tone::Ok, "pass") } else { (Icon::TriangleAlert, Tone::Danger, "harmful") };
        let title = row![tinted(icon, TextRole::Caption, tone), label(c.check.label(), TextRole::Caption, Tone::Normal), label(verdict, TextRole::Caption, tone)].spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center);
        let details = (!c.details.is_empty()).then(|| container(folded(None, c.details.iter().map(String::as_str), DETAIL_LINES, "")).padding(iced::Padding::ZERO.left(sp(SpaceRole::L))));
        column![title, details].into()
    });
    column![head].extend(checks).spacing(sp(SpaceRole::S)).into()
}

/// A peer's standing: it can't talk without a shared protocol, else its version against this app.
fn standing(p: &PeerStanding) -> (Tone, String) {
    if !p.can_talk {
        return (Tone::Danger, "can't talk: no shared protocol".to_owned());
    }
    match p.standing {
        Standing::Current => (Tone::Ok, "current".to_owned()),
        Standing::Ahead => (Tone::Muted, "ahead of this app".to_owned()),
        Standing::Unknown => (Tone::Muted, "version unknown".to_owned()),
        Standing::Behind { majors, minors, patches } => {
            let (n, one, many) = if majors > 0 { (majors, "major", "majors") } else if minors > 0 { (minors, "minor", "minors") } else { (patches, "patch", "patches") };
            (Tone::Warn, match n { 0 => "behind (prerelease)".to_owned(), 1 => format!("behind by 1 {one}"), n => format!("behind by {n} {many}") })
        }
    }
}

impl Screen for State {
    type Local = Local;
    type Event = Event;

    /// A product with a step running takes no further requests.
    fn reduce(&mut self, m: Local) -> Option<Event> {
        match m {
            Local::Toggle(id) => self.selected = if self.selected.as_ref() == Some(&id) { None } else { Some(id) },
            Local::Start(Event::CheckAll) => {
                let idle: Vec<String> = self.idle().map(|p| p.id.clone()).collect();
                self.busy.extend(idle.iter().map(|id| (id.clone(), Step::Checking)));
                return (!idle.is_empty()).then_some(Event::CheckAll);
            }
            Local::Start(e) => {
                let (id, step) = e.step()?;
                if self.busy.contains_key(id) {
                    return None;
                }
                self.busy.insert(id.to_owned(), step);
                return Some(e);
            }
        }
        None
    }

    /// Instant edits of a listed, idle product.
    fn guard(&self, e: &Event) -> bool {
        match e {
            Event::Choose(id, _) | Event::Unpin(id) => self.idle().any(|p| &p.id == id),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_ui_kit::testkit::click_label;
    use loa_settings::updates::{Active, CheckResult, GateCheck, HeldUpdate, ProtocolStatus, Release};
    use semver::Version;

    fn v(s: &str) -> Version { Version::parse(s).unwrap() }
    fn release(id: &str, version: &str) -> Release { Release { product: id.into(), version: v(version), notes: String::new(), published_at: None, artifact: None } }

    fn product(id: &str, kind: ProductKind, current: Option<&str>) -> ProductStatus {
        ProductStatus {
            id: id.into(), label: id.into(), kind, source: "GitHub can1357/oh-my-pi".into(), current: current.map(v), installed: InstalledStatus::Running, offer: None, newest: None,
            changelog: String::new(), last_check_unix: Some(100), staged: Vec::new(), active: None, report: None, held: None, pending_swap: None, pin: None, waiting_past: None,
            signature: None, restart_required: false, error: None,
        }
    }

    fn report() -> GateReport {
        let checks = vec![CheckResult::harmful(GateCheck::ToolSchemas, vec!["- edit: tool removed".into()]), CheckResult::harmful(GateCheck::ContractTests, vec![]), CheckResult::pass(GateCheck::RpcProtocol, vec![])];
        GateReport { product: "omp".into(), from: v("19.0.0"), to: v("19.1.0"), checks }
    }

    /// omp held by a harmful report, the app with a pending swap and a staged offer, Codex up
    /// to date with a rollback, Claude Code not installed.
    fn data() -> Data {
        let mut omp = product("omp", ProductKind::Omp, Some("19.0.0"));
        (omp.report, omp.held, omp.changelog) = (Some(report()), Some(HeldUpdate { version: v("19.1.0"), report: report(), held_at_unix: 5 }), "- faster startup".into());
        let mut app = product("app", ProductKind::App, Some("0.1.0"));
        (app.pending_swap, app.offer, app.staged, app.signature) = (Some(v("0.2.0")), Some(release("app", "0.2.0")), vec![v("0.2.0")], Some(SignatureStatus::Verified));
        let mut codex = product("codex", ProductKind::Harness("codex".into()), Some("1.0.0"));
        codex.active = Some(Active { current: v("1.0.0"), previous: Some(v("0.9.0")) });
        let mut claude = product("claude", ProductKind::Harness("claude".into()), None);
        (claude.installed, claude.newest, claude.offer) = (InstalledStatus::Missing("not on PATH".into()), Some(v("2.0.14")), Some(release("claude", "2.0.14")));
        let protocol = |family: &str| ProtocolStatus { family: family.into(), shared: Some(1), local: Some(1), peer: Some(1) };
        let peers = vec![PeerStanding {
            node: "laptop".into(), name: Some("laptop".into()), app_version: Some(v("0.1.0")), standing: Standing::Behind { majors: 0, minors: 1, patches: 0 },
            protocols: vec![protocol("control"), protocol("session")], can_talk: true,
        }];
        Data { products: vec![omp, app, codex, claude], github_requests_left: Some(42), peers, turn_running: false, now_unix: 400 }
    }

    #[test]
    fn status_lines_follow_the_first_matching_rule() {
        let d = data();
        let lines: Vec<(Tone, String)> = d.products.iter().map(|p| status(p, false)).collect();
        assert_eq!(lines, [
            (Tone::Danger, "19.1.0 held: 2 harmful checks. Update anyway, pin or wait.".to_owned()),
            (Tone::Ok, "Checks passed; ready to swap to 0.2.0".to_owned()),
            (Tone::Ok, "Up to date".to_owned()),
            (Tone::Muted, "Not installed; latest is 2.0.14".to_owned()),
        ]);
        assert_eq!(status(&d.products[1], true).1, "Checks passed; 0.2.0 swaps in when the running turn ends");
        let mut claude = d.products[3].clone();
        (claude.current, claude.staged) = (Some(v("2.0.0")), vec![v("2.0.14")]);
        assert_eq!(status(&claude, false).1, "2.0.14 downloaded; run the gate checks");
        claude.restart_required = true;
        assert_eq!(status(&claude, false), (Tone::Warn, "Installed; restart the app to use it".to_owned()));
        let mut idle = product("x", ProductKind::Omp, Some("1.0.0"));
        idle.last_check_unix = None;
        assert_eq!(status(&idle, false).1, "Not checked yet");
        assert_eq!(standing(&d.peers[0]), (Tone::Warn, "behind by 1 minor".to_owned()));
    }

    #[test]
    fn steps_mark_products_busy_until_finished() {
        let start = |e| Msg::Local(Local::Start(e));
        let choose = |id: &str, c| Msg::Emit(Event::Choose(id.into(), c));
        // (messages, events that reach the app)
        let cases: Vec<(Vec<Message>, Vec<Event>)> = vec![
            (vec![start(Event::Swap("app".into())), start(Event::Check("app".into())), start(Event::Rollback("codex".into())), choose("app", HoldChoice::Wait)], vec![Event::Swap("app".into()), Event::Rollback("codex".into())]),
            (vec![start(Event::Gate("omp".into())), start(Event::CheckAll), start(Event::CheckAll)], vec![Event::Gate("omp".into()), Event::CheckAll]),
            (vec![choose("omp", HoldChoice::PinCurrent), Msg::Emit(Event::Unpin("nope".into())), Msg::Emit(Event::Swap("app".into())), start(Event::Unpin("omp".into()))], vec![Event::Choose("omp".into(), HoldChoice::PinCurrent)]),
        ];
        for (i, (messages, want)) in cases.into_iter().enumerate() {
            let mut s = State::new(data());
            let events: Vec<Event> = messages.into_iter().filter_map(|m| s.update(m)).collect();
            assert_eq!(events, want, "case {i}");
        }
        let mut s = State::new(data());
        s.update(Msg::Local(Local::Start(Event::Gate("omp".into()))));
        s.update(Msg::Local(Local::Start(Event::CheckAll)));
        assert_eq!((s.busy("omp"), s.busy("codex")), (Some(Step::RunningChecks), Some(Step::Checking)), "a running step is kept");
        s.finish("omp");
        assert_eq!(s.update(Msg::Local(Local::Start(Event::Stage("omp".into())))), Some(Event::Stage("omp".into())));
        s.finish_all();
        assert_eq!(s.busy("codex"), None);
    }

    #[test]
    fn the_screen_offers_each_products_next_step() {
        let mut s = State::new(data());
        let mut events = Vec::new();
        for click in ["Update anyway to 19.1.0", "Pin 19.0.0", "Swap to 0.2.0", "Roll back to 0.9.0", "omp"] {
            events.extend(click_label(s.view(), click).into_iter().filter_map(|m| s.update(m)));
        }
        let choose = |c| Event::Choose("omp".into(), c);
        assert_eq!(events, [choose(HoldChoice::UpdateAnyway), choose(HoldChoice::PinCurrent), Event::Swap("app".into()), Event::Rollback("codex".into())]);
        assert_eq!((s.selected.as_deref(), s.busy("app")), (Some("omp"), Some(Step::Swapping)));
        let mut ui = iced_test::simulator(s.view());
        for text in ["Gate report 19.0.0 → 19.1.0", "2 of 3 checks found something harmful.", "- edit: tool removed", "Adapter contract tests", "- faster startup", "GitHub can1357/oh-my-pi · checked 5m ago", "Swapping…", "1 peer is behind."] {
            assert!(ui.find(text).is_ok(), "missing {text}");
        }
        assert!(ui.find("Download 2.0.14").is_err(), "nothing to update while not installed");
    }
}
