//! The working indicator above the editor while the shown session runs a turn, after omp's
//! status line: a spinning accent loader, a label saying what the agent does, drawn in a
//! shimmer (an accent band sweeping over muted text), and, once the turn runs, its elapsed
//! time with the interrupt hint, muted.

use std::ops::Range;
use std::time::Duration;

use iced::Alignment::Center;
use iced::widget::text::{Span, Wrapping};
use iced::widget::{container, rich_text, row, span, text};
use iced::{Element, Font, Length, Padding, Radians, font};
use loa_core::{CallStatus, Dom, ElementKind, MessageKind, MessageStatus, Node, TurnStatus, attr};
use loa_ui_kit::icons::icon;
use loa_ui_kit::style::{self as kit, Tone};
use loa_ui_kit::theme::{AlphaRole, ColorRole, SpaceRole as Sp, TextRole as T, sp, text_size as ts, tokens};
use loa_view::theme::Icon;
use unicode_segmentation::UnicodeSegmentation;

use super::{Message, shimmer};

/// The label while the turn runs with no call or thinking in progress.
pub(super) const WORKING: &str = "Working…";
const THINKING: &str = "Thinking…";
/// The label while a send starts the session, before its turn runs.
pub(super) const STARTING: &str = "Starting session…";
/// The trailer's key hint: Esc interrupts while the composer is busy.
const INTERRUPT_HINT: &str = "esc to interrupt";
/// Most characters of a call title the label shows.
const MAX_TITLE: usize = 60;
/// Graphemes from the middle of the shimmer band to its ends; its middle third is brightest.
const BAND: f32 = 3.0;
/// Spinner turns per shimmer cycle: one a second.
const SPINS: f32 = 1.8;
/// How often the indicator ticks while the window has focus: often enough for the spinner,
/// rarely enough that redrawing the app for a whole turn stays cheap.
pub(super) const FRAME: Duration = Duration::from_millis(90);
/// How often it ticks without focus: the elapsed time keeps counting, the animation rests.
pub(super) const UNFOCUSED: Duration = Duration::from_secs(1);

/// What the shown session's agent is doing, read from its latest turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Activity {
    /// No turn runs, or the session has none yet.
    #[default]
    Idle,
    /// The turn waits on a decision, whose card already asks the user.
    Waiting,
    /// The turn runs: what it does now, and when it started (Unix milliseconds).
    Working { label: String, started: Option<i64> },
}

impl Activity {
    /// The activity of `dom`'s latest turn. A running turn shows its newest running call (its
    /// title, else its tool) or streaming message ("Thinking…" for thinking), else "Working…".
    pub fn of(dom: &Dom) -> Self {
        let Some(turn) = dom.root().and_then(|root| newest_first(dom, root).find(|node| node.kind == ElementKind::Turn)) else { return Self::Idle };
        match turn.str_attr(attr::STATUS).and_then(TurnStatus::parse) {
            Some(TurnStatus::Running) => {}
            Some(TurnStatus::Waiting) => return Self::Waiting,
            _ => return Self::Idle,
        }
        let label = newest_first(dom, turn).find_map(|node| match node.kind {
            ElementKind::Call if node.str_attr(attr::STATUS).and_then(CallStatus::parse) == Some(CallStatus::Running) => {
                let title = node.str_attr(attr::TITLE).and_then(|title| title.lines().next()).map(str::trim).filter(|title| !title.is_empty());
                Some(ongoing(title.or(node.str_attr(attr::TOOL)).unwrap_or_default()))
            }
            ElementKind::Message if node.str_attr(attr::STATUS).and_then(MessageStatus::parse) == Some(MessageStatus::Streaming) => {
                let thinking = node.str_attr(attr::KIND).and_then(MessageKind::parse) == Some(MessageKind::Thinking);
                Some(if thinking { THINKING } else { WORKING }.to_owned())
            }
            _ => None,
        });
        Self::Working { label: label.unwrap_or_else(|| WORKING.to_owned()), started: turn.int_attr(attr::STARTED) }
    }
}

/// The children of `node`, newest first.
fn newest_first<'a>(dom: &'a Dom, node: &'a Node) -> impl Iterator<Item = &'a Node> { node.children.iter().rev().filter_map(|id| dom.get(*id)) }

/// `what` cut to [`MAX_TITLE`] characters, ending in one `…`.
fn ongoing(what: &str) -> String {
    let cut = loa_ui_kit::text::truncate_trailing(what, MAX_TITLE);
    format!("{}…", cut.trim_end_matches(['…', '.', ' ']))
}

/// `12s`, `1m 05s` or `1h 02m` for a turn that has run `ms` milliseconds.
fn elapsed(ms: i64) -> String {
    let s = ms.max(0) / 1000;
    match s {
        ..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

/// The trailer of a running turn: the time since `started` at `now` (both Unix milliseconds),
/// when known, and the interrupt hint.
pub(super) fn trailer(started: Option<i64>, now: i64) -> String {
    started.map_or_else(|| INTERRUPT_HINT.to_owned(), |started| format!("{} · {INTERRUPT_HINT}", elapsed(now - started)))
}

/// The indicator row: the spinner and `label` at shimmer `phase`, then the `trailer`, muted.
/// One line high whatever it shows.
///
/// Accessibility: role `status`, label "<label>, <elapsed>"; the spinner is decorative.
pub(super) fn view(label: &str, trailer: Option<String>, phase: u8) -> Element<'_, Message> {
    let progress = shimmer::progress(phase);
    let spinner = icon(Icon::LoaderCircle, ts(T::Label)).style(kit::tint(Tone::Accent)).rotation(Radians(std::f32::consts::TAU * (progress * SPINS).fract()));
    let t = tokens();
    let accent = t.color(ColorRole::Accent);
    let colors = [t.color(ColorRole::Text2), accent.scale_alpha(t.alpha(AlphaRole::Muted)), accent];
    let center = progress * (label.graphemes(true).count() as f32 + 2.0 * BAND) - BAND;
    let spans: Vec<Span<'_, (), Font>> = runs(label, center).into_iter().map(|(range, level)| span(&label[range]).color(colors[level])).collect();
    let bold = Font { weight: font::Weight::Semibold, ..Font::DEFAULT };
    let label = rich_text(spans).size(ts(T::Label)).font(bold).wrapping(Wrapping::None);
    let trailer = trailer.map(|trailer| text(trailer).size(ts(T::Caption)).style(kit::text(Tone::Muted)).wrapping(Wrapping::None));
    let line = row![spinner, label].push(trailer).spacing(sp(Sp::S)).align_y(Center);
    container(line).width(Length::Fill).clip(true).padding(Padding { bottom: 0.0, ..Padding::new(sp(Sp::M)) }).into()
}

/// `label` in runs of one shimmer level for a band centered on grapheme `center`: 0 off the
/// band, 1 on its edges, 2 in its middle. Runs break only between graphemes, so a colour
/// change never splits a letter from its combining marks.
fn runs(label: &str, center: f32) -> Vec<(Range<usize>, usize)> {
    let mut runs: Vec<(Range<usize>, usize)> = Vec::new();
    for (index, (at, grapheme)) in label.grapheme_indices(true).enumerate() {
        let distance = (index as f32 - center).abs();
        let level = usize::from(distance < BAND) + usize::from(distance < BAND / 3.0);
        match runs.last_mut() {
            Some((range, last)) if *last == level => range.end = at + grapheme.len(),
            _ => runs.push((at..at + grapheme.len(), level)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_core::testkit::DomFixture;
    use loa_core::{Draft, Role, attrs};

    type Build = fn(&mut DomFixture);

    /// A session whose latest turn has `status`, started at 1000, with a user message and then
    /// what `build` adds.
    fn session(status: TurnStatus, build: &[Build]) -> Dom {
        let mut f = DomFixture::session();
        let earlier = f.turn().last;
        f.commit(Draft::set(earlier, attr::STATUS, TurnStatus::Done));
        let turn = f.turn().last;
        f.commit(Draft::set(turn, attr::STATUS, status)).commit(Draft::set(turn, attr::STARTED, 1000i64)).message(Role::User, "hi");
        build.iter().for_each(|build| build(&mut f));
        f.build()
    }

    fn streaming(f: &mut DomFixture, kind: MessageKind) {
        let message = f.message(Role::Assistant, "").last;
        f.commit(Draft::set(message, attr::KIND, kind)).commit(Draft::set(message, attr::STATUS, MessageStatus::Streaming));
    }

    fn working(label: &str) -> Activity { Activity::Working { label: label.to_owned(), started: Some(1000) } }

    #[test]
    fn the_activity_follows_the_latest_turn() {
        let reading: Build = |f| { f.call("read", attrs! { STATUS: "running", TITLE: "Reading the config\nand more" }); };
        let bash: Build = |f| { f.call("bash", attrs! { STATUS: "running" }); };
        let searched: Build = |f| { f.call("grep", attrs! { STATUS: "done", TITLE: "Searching" }); };
        let thinking: Build = |f| streaming(f, MessageKind::Thinking);
        let text: Build = |f| streaming(f, MessageKind::Text);
        let cases: [(TurnStatus, &[Build], Activity); 10] = [
            (TurnStatus::Running, &[reading], working("Reading the config…")),
            (TurnStatus::Running, &[reading, bash], working("bash…")),
            (TurnStatus::Running, &[thinking], working(THINKING)),
            (TurnStatus::Running, &[thinking, bash], working("bash…")),
            (TurnStatus::Running, &[bash, text], working(WORKING)),
            (TurnStatus::Running, &[searched], working(WORKING)),
            (TurnStatus::Running, &[], working(WORKING)),
            (TurnStatus::Waiting, &[bash], Activity::Waiting),
            (TurnStatus::Done, &[thinking], Activity::Idle),
            (TurnStatus::Cancelled, &[bash], Activity::Idle),
        ];
        for (n, (status, build, expected)) in cases.into_iter().enumerate() {
            assert_eq!(Activity::of(&session(status, build)), expected, "case {n}");
        }
        assert_eq!(Activity::of(&Dom::new()), Activity::Idle);
    }

    #[test]
    fn a_long_title_is_cut_and_ends_in_one_ellipsis() {
        assert_eq!(ongoing("Reading..."), "Reading…");
        let long = ongoing(&"a".repeat(100));
        assert_eq!((long.chars().count(), long.ends_with("a…")), (MAX_TITLE, true));
    }

    #[test]
    fn elapsed_time_reads_in_seconds_minutes_or_hours() {
        let table = [(-5, "0s"), (999, "0s"), (12_400, "12s"), (65_000, "1m 05s"), (3_599_000, "59m 59s"), (3_720_000, "1h 02m")];
        for (ms, expected) in table {
            assert_eq!(elapsed(ms), expected, "{ms}");
        }
    }

    #[test]
    fn the_shimmer_band_brightens_the_graphemes_around_its_center() {
        let label = "Working…";
        let levels = |label: &'static str, center| runs(label, center).into_iter().map(|(range, level)| (&label[range], level)).collect::<Vec<_>>();
        assert_eq!(levels(label, -BAND), [(label, 0)], "the band starts off the label");
        assert_eq!(levels(label, 3.0), [("W", 0), ("or", 1), ("k", 2), ("in", 1), ("g…", 0)]);
        assert_eq!(levels("ae\u{301}x", 1.0), [("a", 1), ("e\u{301}", 2), ("x", 1)], "an accent changes colour with its letter");
    }
}
