//! Diff view with unified and side by side modes.
//!
//! Feed it [`FileDiff`]s built from old and new text
//! ([`FileDiff::from_texts`]) or parsed from a unified diff
//! ([`parse_unified`]). Unchanged runs fold away and expand on click. Rows have
//! a fixed height and only the ones in the viewport are built, so a 50k line
//! diff costs the same per frame as a short one.

mod model;
mod parse;

pub use model::{CONTEXT, FileDiff, FileStatus, Fold, Hunk, Intra, Line, MIN_FOLD, Row, Tag, intra_line};
pub use parse::{ParseError, Result, parse_unified};

use crate::style;
use iced::widget::{Id, button, column, container, operation, row, rule, scrollable, space, text};
use iced::{Element, Font, Length, Task};
use loa_ui_icons::Icon;
use std::ops::Range;

/// Rows built above and below the viewport so fast scrolling doesn't show
/// gaps before the next frame.
const OVERSCAN: usize = 20;

/// Viewport height assumed before the first scroll event reports it.
const DEFAULT_VIEWPORT: f32 = 1200.0;

/// Width of one monospace character at [`style::TEXT_SIZE`].
const CHAR_WIDTH: f32 = 8.0;

/// Height of the file list when a diff has several files.
const FILE_LIST_HEIGHT: f32 = 140.0;

/// Unified (one column with `-`/`+` lines) or split (old left, new right).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Mode {
    #[default]
    Unified,
    Split,
}

#[derive(Debug, Clone)]
pub enum Message {
    SelectFile(usize),
    SetMode(Mode),
    /// Expand a fold of the selected file.
    Expand(usize),
    Scrolled { offset: f32, height: f32 },
    /// Open the selected file in the editor or files tab.
    Open,
    /// Show or hide the lines of the selected file under its header.
    ToggleCollapsed,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The user asked to open this path (the new path, or the old one for a
    /// deleted file).
    OpenFile(String),
}

/// State of the diff tab.
#[derive(Debug)]
pub struct State {
    files: Vec<FileDiff>,
    selected: usize,
    mode: Mode,
    /// Rows of the selected file in the current mode and fold state.
    rows: Vec<Row>,
    scroll_offset: f32,
    viewport_height: f32,
    scroll_id: Id,
    /// The selected file's lines are hidden; only its header shows.
    collapsed: bool,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            selected: 0,
            mode: Mode::Unified,
            rows: Vec::new(),
            scroll_offset: 0.0,
            viewport_height: DEFAULT_VIEWPORT,
            scroll_id: Id::unique(),
            collapsed: false,
        }
    }

    /// Replaces the files shown and selects the first.
    pub fn set_files(&mut self, files: Vec<FileDiff>) -> Task<Message> {
        self.files = files;
        self.select(0)
    }

    pub fn files(&self) -> &[FileDiff] {
        &self.files
    }

    pub fn selected(&self) -> Option<&FileDiff> {
        self.files.get(self.selected)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Every row of the selected file.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Indices of the rows the view builds for the current scroll position.
    pub fn visible_rows(&self) -> Range<usize> {
        let first = (self.scroll_offset / style::ROW_HEIGHT).floor().max(0.0) as usize;
        let shown = (self.viewport_height / style::ROW_HEIGHT).ceil().max(1.0) as usize;
        let start = first.saturating_sub(OVERSCAN).min(self.rows.len());
        let end = (first + shown + OVERSCAN).min(self.rows.len());
        start..end
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::SelectFile(index) if index < self.files.len() && index != self.selected => {
                (self.select(index), None)
            }
            Message::SelectFile(_) => (Task::none(), None),
            Message::SetMode(mode) => {
                if mode != self.mode {
                    // Keep the same part of the file in view: rows map to a
                    // similar fraction in both modes.
                    let fraction = self.scroll_offset / (self.rows.len().max(1) as f32 * style::ROW_HEIGHT);
                    self.mode = mode;
                    self.rebuild_rows();
                    self.scroll_offset = fraction * self.rows.len() as f32 * style::ROW_HEIGHT;
                }
                (Task::none(), None)
            }
            Message::Expand(fold) => {
                if let Some(file) = self.files.get_mut(self.selected)
                    && file.expand(fold)
                {
                    self.rebuild_rows();
                }
                (Task::none(), None)
            }
            Message::Scrolled { offset, height } => {
                self.scroll_offset = offset;
                self.viewport_height = height;
                (Task::none(), None)
            }
            Message::Open => {
                let event = self.selected().map(|file| Event::OpenFile(file.path().to_owned()));
                (Task::none(), event)
            }
            Message::ToggleCollapsed => {
                self.collapsed = !self.collapsed;
                (Task::none(), None)
            }
        }
    }

    fn select(&mut self, index: usize) -> Task<Message> {
        self.selected = index;
        self.collapsed = false;
        self.scroll_offset = 0.0;
        self.rebuild_rows();
        operation::snap_to(self.scroll_id.clone(), scrollable::RelativeOffset::START)
    }

    fn rebuild_rows(&mut self) {
        self.rows = match (self.files.get(self.selected), self.mode) {
            (None, _) => Vec::new(),
            (Some(file), Mode::Unified) => file.unified_rows(),
            (Some(file), Mode::Split) => file.split_rows(),
        };
    }

    pub fn view(&self) -> Element<'_, Message> {
        let Some(file) = self.selected() else {
            return container(text("No changes").style(style::muted)).padding(8).into();
        };
        let mut page = column![self.toolbar(file), rule::horizontal(1).style(style::divider)];
        if self.files.len() > 1 {
            page = page.push(self.file_list());
        }
        let body: Element<'_, Message> = if self.collapsed {
            space().into()
        } else if file.binary {
            container(text("Binary file").style(style::muted)).padding(8).into()
        } else {
            self.body(file)
        };
        page.push(body).into()
    }

    /// The file header: collapse chevron and path on the left, change counts
    /// and the unified/split switch on the right.
    fn toolbar<'a>(&'a self, file: &'a FileDiff) -> Element<'a, Message> {
        let chevron = if self.collapsed { Icon::ChevronRight } else { Icon::ChevronDown };
        let collapse = button(loa_ui_icons::muted(chevron, 14.0))
            .padding(2)
            .style(style::row_button(false))
            .on_press(Message::ToggleCollapsed);
        let path = button(text(file.path()).size(style::LIST_TEXT_SIZE).wrapping(text::Wrapping::None))
            .padding([2, 4])
            .style(style::row_button(false))
            .on_press(Message::Open);
        let (other, label) = match self.mode {
            Mode::Unified => (Mode::Split, "Split"),
            Mode::Split => (Mode::Unified, "Unified"),
        };
        let mode = button(text(label).size(12).style(style::muted))
            .padding([2, 6])
            .style(style::row_button(false))
            .on_press(Message::SetMode(other));
        row![
            collapse,
            path,
            space::horizontal(),
            text(format!("+{}", file.added())).size(12).style(style::success_text),
            text(format!("-{}", file.removed())).size(12).style(style::danger_text),
            mode,
        ]
        .spacing(4)
        .padding([5, 8])
        .align_y(iced::Alignment::Center)
        .into()
    }

    fn file_list(&self) -> Element<'_, Message> {
        let entries = self.files.iter().enumerate().map(|(index, file)| {
            let marker = match file.status() {
                FileStatus::Added => "A",
                FileStatus::Deleted => "D",
                FileStatus::Modified => "M",
                FileStatus::Renamed => "R",
            };
            let label = row![
                text(marker).size(style::TEXT_SIZE).font(Font::MONOSPACE).style(style::muted),
                text(file.path()).size(style::TEXT_SIZE).wrapping(text::Wrapping::None).width(Length::Fill),
                text(format!("+{}", file.added())).size(style::TEXT_SIZE).style(style::success_text),
                text(format!("-{}", file.removed())).size(style::TEXT_SIZE).style(style::danger_text),
            ]
            .spacing(6);
            button(label)
                .width(Length::Fill)
                .height(style::ROW_HEIGHT)
                .padding([1, 6])
                .style(style::row_button(index == self.selected))
                .on_press(Message::SelectFile(index))
                .into()
        });
        let list_height = (self.files.len() as f32 * style::ROW_HEIGHT).min(FILE_LIST_HEIGHT);
        scrollable(column(entries)).height(list_height).into()
    }

    fn body<'a>(&'a self, file: &'a FileDiff) -> Element<'a, Message> {
        let range = self.visible_rows();
        let digits = number_digits(file);
        let above = range.start as f32 * style::ROW_HEIGHT;
        let below = (self.rows.len() - range.end) as f32 * style::ROW_HEIGHT;
        let rows = self.rows[range].iter().map(|row| self.row(file, *row, digits));
        let content = column![space().height(above), column(rows), space().height(below)].width(Length::Fill);
        scrollable(content)
            .id(self.scroll_id.clone())
            .height(Length::Fill)
            .on_scroll(|viewport| Message::Scrolled {
                offset: viewport.absolute_offset().y,
                height: viewport.bounds().height,
            })
            .into()
    }

    fn row<'a>(&'a self, file: &'a FileDiff, row: Row, digits: usize) -> Element<'a, Message> {
        let line = match row {
            Row::Hunk(hunk) => {
                let header = format!(
                    "@@ -{},{} +{},{} @@ {}",
                    hunk.old_start,
                    hunk.old_len,
                    hunk.new_start,
                    hunk.new_len,
                    file.section(&hunk)
                );
                container(text(header).size(12).wrapping(text::Wrapping::None).style(style::muted))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .align_y(iced::Center)
                    .style(style::band)
                    .padding([0, 10])
                    .into()
            }
            Row::Fold { fold, hidden } => {
                let label = if hidden == 1 { "Expand 1 unchanged line".to_owned() } else {
                    format!("Expand {hidden} unchanged lines")
                };
                button(mono(label).style(style::muted))
                    .width(Length::Fill)
                    .padding([0, 6])
                    .style(style::row_button(false))
                    .on_press(Message::Expand(fold))
                    .into()
            }
            Row::Line(index) => unified_line(file, index, digits),
            Row::Pair { old, new } => row![
                split_side(file, old, Side::Old, digits),
                split_side(file, new, Side::New, digits)
            ]
            .into(),
        };
        container(line).width(Length::Fill).height(style::ROW_HEIGHT).clip(true).into()
    }
}

#[derive(Copy, Clone)]
enum Side {
    Old,
    New,
}

fn mono<'a>(content: impl text::IntoFragment<'a>) -> text::Text<'a> {
    text(content).size(style::TEXT_SIZE).font(Font::MONOSPACE).wrapping(text::Wrapping::None)
}

/// Digits needed for the largest line number of the file.
fn number_digits(file: &FileDiff) -> usize {
    let max = file.lines().iter().flat_map(|l| l.old_no.max(l.new_no)).max().unwrap_or(0);
    max.max(1).ilog10() as usize + 1
}

fn number<'a>(no: Option<u32>, digits: usize) -> Element<'a, Message> {
    let label = no.map_or_else(String::new, |n| n.to_string());
    container(mono(label).style(style::muted))
        .width(digits as f32 * CHAR_WIDTH + 16.0)
        .align_x(iced::alignment::Horizontal::Right)
        .into()
}

fn line_style(tag: Tag) -> Option<fn(&iced::Theme) -> container::Style> {
    match tag {
        Tag::Equal => None,
        Tag::Delete => Some(style::removed_line),
        Tag::Insert => Some(style::added_line),
    }
}

fn unified_line(file: &FileDiff, index: usize, digits: usize) -> Element<'_, Message> {
    let line = &file.lines()[index];
    let sign = match line.tag {
        Tag::Equal => " ",
        Tag::Delete => "-",
        Tag::Insert => "+",
    };
    let content = row![
        number(line.old_no, digits),
        number(line.new_no, digits),
        container(mono(sign).style(sign_style(line.tag))).padding(iced::Padding::ZERO.left(12).right(6)),
        line_content(file, index),
    ];
    paint(content.into(), line.tag)
}

fn sign_style(tag: Tag) -> fn(&iced::Theme) -> text::Style {
    match tag {
        Tag::Equal => style::muted,
        Tag::Delete => style::danger_text,
        Tag::Insert => style::success_text,
    }
}

fn split_side(file: &FileDiff, index: Option<usize>, side: Side, digits: usize) -> Element<'_, Message> {
    let Some(index) = index else {
        return container(space()).width(Length::FillPortion(1)).height(Length::Fill).style(style::band).into();
    };
    let line = &file.lines()[index];
    let no = match side {
        Side::Old => line.old_no,
        Side::New => line.new_no,
    };
    let content = row![number(no, digits), line_content(file, index)];
    container(paint(content.into(), line.tag)).width(Length::FillPortion(1)).clip(true).into()
}

fn paint(content: Element<'_, Message>, tag: Tag) -> Element<'_, Message> {
    let painted = container(content).width(Length::Fill).height(Length::Fill);
    match line_style(tag) {
        Some(style) => painted.style(style).into(),
        None => painted.into(),
    }
}

/// The line text, with the intra-line changed parts on a stronger background.
fn line_content(file: &FileDiff, index: usize) -> Element<'_, Message> {
    let content = file.line_text(index);
    let emphasis = file.emphasis(index);
    if emphasis.is_empty() {
        return mono(content).into();
    }
    let strong = match file.lines()[index].tag {
        Tag::Insert => style::added_emphasis,
        _ => style::removed_emphasis,
    };
    let mut parts = row![];
    let mut at = 0;
    for range in emphasis {
        if range.start > at {
            parts = parts.push(mono(&content[at..range.start]));
        }
        parts = parts.push(container(mono(&content[range.clone()])).style(strong));
        at = range.end;
    }
    if at < content.len() {
        parts = parts.push(mono(&content[at..]));
    }
    parts.into()
}

#[cfg(test)]
mod tests;
