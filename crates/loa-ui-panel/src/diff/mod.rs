//! The diff tab: unified and split views of [`parse_unified`] output with folds, building only the rows in view.

mod model;
mod parse;

pub use model::{FileDiff, FileStatus, Hunk, Line, Row, Tag};
pub use parse::{ParseError, Result, parse_unified};

use crate::{row_height, tint};
use iced::widget::{Id, button, column, container, operation, row, rule, scrollable, space, text};
use iced::{Element, Fill, Font, Padding, Task};
use loa_ui_kit::icons::muted;
use loa_view::panel::DiffIntent;
use loa_view::theme::Icon;
use loa_ui_kit::style::{self as kit, ButtonKind, Tone};
use loa_ui_kit::theme::{ColorRole as C, SpaceRole as Sp, TextRole as T, pad_x, sp, text_size as ts};
use loa_ui_kit::widgets::label;
use std::ops::Range;

/// Rows built beyond the viewport so fast scrolling shows no gaps.
const OVERSCAN: usize = 20;
/// Viewport height assumed until the first scroll event reports it.
const DEFAULT_VIEWPORT: f32 = 1200.0;
/// Tallest the file list grows when a diff has several files.
const FILE_LIST_HEIGHT: f32 = 140.0;

fn file_row_height() -> f32 { ts(T::Label) + 2.0 * sp(Sp::Xs) }
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Mode { #[default] Unified, Split }
/// `Expand` opens a fold of the selected file, `Open` the file itself; `ToggleCollapsed` hides its lines.
#[derive(Debug, Clone)]
pub enum Message { SelectFile(usize), SetMode(Mode), Expand(usize), Scrolled { offset: f32, height: f32 }, Open, ToggleCollapsed }
/// The files, the selected one's rows in the current mode, the scroll position and a note replacing "No changes".
#[derive(Debug)]
pub struct State {
    files: Vec<FileDiff>, selected: usize, mode: Mode, rows: Vec<Row>,
    scroll_offset: f32, viewport_height: f32, scroll_id: Id, collapsed: bool, note: Option<String>,
}
impl Default for State {
    fn default() -> Self { Self { files: Vec::new(), selected: 0, mode: Mode::Unified, rows: Vec::new(), scroll_offset: 0.0, viewport_height: DEFAULT_VIEWPORT, scroll_id: Id::unique(), collapsed: false, note: None } }
}

impl State {
    pub fn new() -> Self { Self::default() }
    /// Replaces "No changes", e.g. outside a git repository, and clears the files.
    pub fn set_note(&mut self, note: Option<String>) -> Task<Message> {
        self.note = note;
        if self.note.is_some() { self.set_files(Vec::new()) } else { Task::none() }
    }
    /// Replaces the files and selects the first.
    pub fn set_files(&mut self, files: Vec<FileDiff>) -> Task<Message> {
        self.files = files;
        self.select(0)
    }
    pub fn files(&self) -> &[FileDiff] { &self.files }
    pub fn selected(&self) -> Option<&FileDiff> { self.files.get(self.selected) }
    /// The rows the view builds at the current scroll position.
    fn visible_rows(&self) -> Range<usize> {
        let first = (self.scroll_offset / row_height()).floor().max(0.0) as usize;
        let shown = (self.viewport_height / row_height()).ceil().max(1.0) as usize;
        first.saturating_sub(OVERSCAN).min(self.rows.len())..(first + shown + OVERSCAN).min(self.rows.len())
    }
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<DiffIntent>) {
        match message {
            Message::SelectFile(i) if i < self.files.len() && i != self.selected => return (self.select(i), None),
            Message::SetMode(mode) if mode != self.mode => {
                // Rows map to a similar fraction in both modes, so the same part stays in view.
                let fraction = self.scroll_offset / (self.rows.len().max(1) as f32 * row_height());
                self.mode = mode;
                self.rebuild_rows();
                self.scroll_offset = fraction * self.rows.len() as f32 * row_height();
            }
            Message::SelectFile(_) | Message::SetMode(_) => {}
            Message::Expand(fold) => {
                if self.files.get_mut(self.selected).is_some_and(|f| f.expand(fold)) { self.rebuild_rows() }
            }
            Message::Scrolled { offset, height } => (self.scroll_offset, self.viewport_height) = (offset, height),
            Message::Open => return (Task::none(), self.selected().map(|f| DiffIntent::OpenFile(f.path().to_owned()))),
            Message::ToggleCollapsed => self.collapsed = !self.collapsed,
        }
        (Task::none(), None)
    }
    fn select(&mut self, index: usize) -> Task<Message> {
        (self.selected, self.collapsed, self.scroll_offset) = (index, false, 0.0);
        self.rebuild_rows();
        operation::snap_to(self.scroll_id.clone(), scrollable::RelativeOffset::START)
    }
    fn rebuild_rows(&mut self) { self.rows = self.files.get(self.selected).map_or_else(Vec::new, |f| if self.mode == Mode::Unified { f.unified_rows() } else { f.split_rows() }) }
    pub fn view(&self) -> Element<'_, Message> {
        let note = |s| container(label(s, T::Body, Tone::Muted)).padding(sp(Sp::S));
        let Some(file) = self.selected() else { return note(self.note.as_deref().unwrap_or("No changes")).into() };
        let body: Element<'_, Message> = match (self.collapsed, file.binary) {
            (true, _) => space().into(),
            (_, true) => note("Binary file").into(),
            _ => self.body(file),
        };
        column![self.toolbar(file), rule::horizontal(1).style(kit::rule), (self.files.len() > 1).then(|| self.file_list()), body].into()
    }
    /// Collapse chevron and path, then the change counts and the unified/split switch.
    fn toolbar<'a>(&'a self, file: &'a FileDiff) -> Element<'a, Message> {
        let flat = |content: Element<'a, Message>, on| button(content).padding([sp(Sp::Xs), sp(Sp::S)]).style(kit::button(ButtonKind::Row { selected: false })).on_press(on);
        let chevron = muted(if self.collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, ts(T::Body));
        let other = if self.mode == Mode::Unified { (Mode::Split, "Split") } else { (Mode::Unified, "Unified") };
        let counts = [label(format!("+{}", file.added()), T::Caption, Tone::Ok), label(format!("-{}", file.removed()), T::Caption, Tone::Danger)].map(Element::from);
        let head = row![flat(chevron.into(), Message::ToggleCollapsed), flat(text(file.path()).size(ts(T::Body)).wrapping(text::Wrapping::None).into(), Message::Open), space::horizontal()];
        head.extend(counts).push(flat(label(other.1, T::Caption, Tone::Muted).into(), Message::SetMode(other.0))).spacing(sp(Sp::Xs)).padding([sp(Sp::Xs), sp(Sp::S)]).align_y(iced::Alignment::Center).into()
    }
    fn file_list(&self) -> Element<'_, Message> {
        let small = |s: String, tone| label(s, T::Label, tone).line_height(1.0);
        let entries = self.files.iter().enumerate().map(|(i, f)| {
            let marker = ["A", "D", "M", "R"][f.status() as usize];
            let line = row![small(marker.into(), Tone::Muted).font(Font::MONOSPACE), small(f.path().into(), Tone::Normal).wrapping(text::Wrapping::None).width(Fill), small(format!("+{}", f.added()), Tone::Ok), small(format!("-{}", f.removed()), Tone::Danger)];
            let style = kit::button(ButtonKind::Row { selected: i == self.selected });
            button(line.spacing(sp(Sp::S))).width(Fill).height(file_row_height()).padding([sp(Sp::Xs), sp(Sp::S)]).style(style).on_press(Message::SelectFile(i)).into()
        });
        scrollable(column(entries)).height((self.files.len() as f32 * file_row_height()).min(FILE_LIST_HEIGHT)).into()
    }
    /// The rows in view between spacers standing in for the rest.
    fn body<'a>(&'a self, file: &'a FileDiff) -> Element<'a, Message> {
        let range = self.visible_rows();
        let digits = file.max_line_no().max(1).ilog10() as usize + 1;
        let (above, below) = (range.start as f32 * row_height(), (self.rows.len() - range.end) as f32 * row_height());
        let rows = self.rows[range].iter().map(|r| container(row_view(file, *r, digits)).width(Fill).height(row_height()).clip(true).into());
        let content = column![space().height(above), column(rows), space().height(below)].width(Fill);
        scrollable(content).id(self.scroll_id.clone()).height(Fill).on_scroll(|v| Message::Scrolled { offset: v.absolute_offset().y, height: v.bounds().height }).into()
    }
}

panel_pane!("diff", "Diff", GitCompare);

fn mono<'a>(content: impl text::IntoFragment<'a>) -> text::Text<'a> { text(content).size(ts(T::Mono)).line_height(1.0).font(Font::MONOSPACE).wrapping(text::Wrapping::None) }
fn muted_mono<'a>(content: impl text::IntoFragment<'a>) -> text::Text<'a> { mono(content).style(kit::text(Tone::Muted)) }
fn paint(content: Element<'_, Message>, tag: Tag) -> Element<'_, Message> {
    let painted = container(content).width(Fill).height(Fill);
    if tag == Tag::Equal { painted.into() } else { painted.style(kit::fill([C::RemovedBg, C::AddedBg][tag as usize - 1], 0.0)).into() }
}

fn row_view(file: &FileDiff, r: Row, digits: usize) -> Element<'_, Message> {
    let number = |no: Option<u32>| container(muted_mono(no.map_or_else(String::new, |n| n.to_string()))).width(digits as f32 * ts(T::Mono) * 0.6 + sp(Sp::L)).align_x(iced::alignment::Horizontal::Right);
    match r {
        Row::Hunk(h) => {
            let header = format!("@@ -{},{} +{},{} @@ {}", h.old_start, h.old_len, h.new_start, h.new_len, file.section(&h));
            container(muted_mono(header)).width(Fill).height(Fill).align_y(iced::Center).style(kit::fill(C::Raised, 0.0)).padding(pad_x(Sp::M)).into()
        }
        Row::Fold { fold, hidden } => {
            let s = if hidden == 1 { "Expand 1 unchanged line".to_owned() } else { format!("Expand {hidden} unchanged lines") };
            button(muted_mono(s)).width(Fill).padding(pad_x(Sp::S)).style(kit::button(ButtonKind::Row { selected: false })).on_press(Message::Expand(fold)).into()
        }
        Row::Line(i) => {
            let l = &file.lines()[i];
            let (sign, tone) = [(" ", Tone::Muted), ("-", Tone::Danger), ("+", Tone::Ok)][l.tag as usize];
            let sign = container(mono(sign).style(kit::text(tone))).padding(Padding::ZERO.left(sp(Sp::M)).right(sp(Sp::S)));
            paint(row![number(l.old_no), number(l.new_no), sign, line_content(file, i)].into(), l.tag)
        }
        Row::Pair { old, new } => {
            let side = |i: Option<usize>, no: fn(&Line) -> Option<u32>| -> Element<'_, Message> {
                let Some(i) = i else { return container(space()).width(Fill).height(Fill).style(kit::fill(C::Raised, 0.0)).into() };
                let l = &file.lines()[i];
                container(paint(row![number(no(l)), line_content(file, i)].into(), l.tag)).width(Fill).clip(true).into()
            };
            row![side(old, |l| l.old_no), side(new, |l| l.new_no)].into()
        }
    }
}

/// The line text with its changed words on a stronger background.
fn line_content(file: &FileDiff, i: usize) -> Element<'_, Message> {
    let (content, emphasis) = (file.line_text(i), file.emphasis(i));
    if emphasis.is_empty() { return mono(content).into() }
    let strong = if file.lines()[i].tag == Tag::Insert { C::Ok } else { C::Danger };
    let (mut parts, mut at) = (row![], 0);
    for r in emphasis {
        parts = parts.push((r.start > at).then(|| mono(&content[at..r.start]))).push(container(mono(&content[r.clone()])).style(tint(strong)));
        at = r.end;
    }
    parts.push((at < content.len()).then(|| mono(&content[at..]))).into()
}

#[cfg(test)]
mod tests;
