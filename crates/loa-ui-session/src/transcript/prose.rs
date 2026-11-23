//! Assistant Markdown: parsed incrementally while it streams, and drawn in the transcript's look.

use super::copy_button::copy_button_with;
use super::selection::Group;
use super::Message;
use iced::alignment::Vertical;
use iced::widget::markdown::{self, Bullet, HeadingLevel, Item, Settings, Text};
use iced::widget::text::LineHeight;
use iced::widget::{checkbox, column, container, row, scrollable, span, text};
use iced::{Background, Color, Element, Fill, Font, Padding, border};
use loa_ui_kit::style::{self as kit, Surface, Tone};
use loa_ui_kit::theme::{ColorRole as C, RadiusRole, SpaceRole as Sp, TextRole as T, TokenSet, generation, pad_x, sp};
use std::cell::Cell;

/// Line height of body text, relative to its size.
pub(crate) const LINE_HEIGHT: f32 = 1.6;

/// Parsed markdown of one message, fed only the text that arrives.
#[derive(Debug, Default)]
pub struct Streamed {
    content: markdown::Content,
    /// Bytes of message text handed to the parser so far.
    pub fed: usize,
    /// Newlines so far, for height estimates.
    pub lines: usize,
}

impl Streamed {
    pub(crate) fn new(text: &str) -> Self {
        let mut s = Self::default();
        s.push(text);
        s
    }

    fn push(&mut self, chunk: &str) {
        self.content.push_str(&hard_breaks(chunk));
        self.fed += chunk.len();
        self.lines += chunk.bytes().filter(|b| *b == b'\n').count();
    }

    /// Catches up with `text` after `chunk` was appended: pushes the chunk when the parse holds
    /// exactly the text before it, re-parses otherwise.
    pub(crate) fn append(&mut self, text: &str, chunk: &str) {
        if self.fed + chunk.len() == text.len() && text.ends_with(chunk) { self.push(chunk) } else { *self = Self::new(text) }
    }

    pub(crate) fn items(&self) -> &[Item] { self.content.items() }
}

/// Every newline as a markdown hard break, so replies keep the model's line breaks.
pub(crate) fn hard_breaks(text: &str) -> String {
    let mut result = String::with_capacity(text.len() + 2 * text.bytes().filter(|&b| b == b'\n').count());
    for line in text.split_inclusive('\n') {
        if let Some(content) = line.strip_suffix('\n') {
            result.push_str(content.strip_suffix('\r').unwrap_or(content));
            result.push_str("  \n");
        } else {
            result.push_str(line);
        }
    }
    result
}

/// Undo the transcript's hard-break decoration on fenced code only. Keep original whitespace,
/// including blank lines and any spaces the author put before a line break.
fn code_text(code: &str) -> String {
    let mut result = String::with_capacity(code.len());
    for line in code.split_inclusive('\n') {
        if let Some(content) = line.strip_suffix("  \n") {
            result.push_str(content);
            result.push('\n');
        } else if line == " \n" {
            // Indented fences can consume one of the two synthetic spaces on a blank line.
            result.push('\n');
        } else {
            result.push_str(line);
        }
    }
    result
}

/// Colour-taking widget settings of one token generation; rebuilt when it changes.
#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub markdown: Settings,
    pub muted: Color,
    pub generation: u64,
}

impl Look {
    pub(crate) fn new(t: &TokenSet) -> Self {
        let style = markdown::Style {
            font: Font::DEFAULT,
            inline_code_highlight: markdown::Highlight { background: Background::Color(t.color(C::Raised)), border: border::rounded(t.radius(RadiusRole::Pill)) },
            inline_code_padding: Padding::ZERO.left(t.space(Sp::Xs) / 2.0).right(t.space(Sp::Xs) / 2.0),
            inline_code_color: t.color(C::Text),
            inline_code_font: t.font(T::Mono),
            code_block_font: t.font(T::Mono),
            link_color: t.color(C::Accent),
        };
        let mut markdown = Settings::with_text_size(t.text(T::Body), style);
        markdown.code_size = t.text(T::Mono).into();
        markdown.spacing = t.space(Sp::M).into();
        Self { markdown, muted: t.color(C::Text2), generation: generation() }
    }
}

pub(crate) fn view<'a>(items: &'a [Item], source: &'a str, look: Look) -> Element<'a, Message> {
    let selection = Group::default();
    let body = markdown::view_with(items, look.markdown, &Prose { selection: selection.clone(), source, code_index: Cell::new(0) });
    selection.wrap(body)
}

struct Prose<'a> {
    selection: Group,
    source: &'a str,
    code_index: Cell<usize>,
}

fn take_code(items: impl IntoIterator<Item = Item>, index: &mut usize) -> Option<String> {
    for item in items {
        match item {
            Item::CodeBlock { code, .. } => {
                if *index == 0 { return Some(code); }
                *index -= 1;
            }
            Item::List { bullets, .. } => {
                for bullet in bullets {
                    let items = match bullet {
                        Bullet::Point { items } | Bullet::Task { items, .. } => items,
                    };
                    if let Some(code) = take_code(items, index) { return Some(code); }
                }
            }
            Item::Quote(items) => {
                if let Some(code) = take_code(items, index) { return Some(code); }
            }
            _ => {}
        }
    }
    None
}

impl<'a> markdown::Viewer<'a, Message> for Prose<'a> {
    fn on_link_click(url: markdown::Uri) -> Message { Message::Link(url) }

    fn paragraph(&self, settings: Settings, text: &Text) -> Element<'a, Message> {
        let spans = text.spans(settings.style);
        let spans = spans.iter().cloned().map(|s| if s.highlight.is_some() { s.size(settings.code_size) } else { s }).collect();
        self.selection.text(spans, settings.text_size, LINE_HEIGHT.into(), settings.style.font)
    }

    fn heading(&self, settings: Settings, level: &'a HeadingLevel, text: &'a Text, index: usize) -> Element<'a, Message> {
        let size = match level {
            HeadingLevel::H1 => settings.h1_size,
            HeadingLevel::H2 => settings.h2_size,
            HeadingLevel::H3 => settings.h3_size,
            HeadingLevel::H4 => settings.h4_size,
            HeadingLevel::H5 => settings.h5_size,
            HeadingLevel::H6 => settings.h6_size,
        };
        let heading = self.selection.text(text.spans(settings.style).to_vec(), size, LineHeight::default(), settings.style.font);
        container(heading).padding(Padding::ZERO.top(if index == 0 { 0.0 } else { settings.text_size.0 / 2.0 })).into()
    }

    fn unordered_list(&self, settings: Settings, bullets: &'a [Bullet]) -> Element<'a, Message> {
        let line = settings.text_size.0 * LINE_HEIGHT;
        let inner = Settings { spacing: settings.spacing * 0.5, ..settings };
        column(bullets.iter().map(|bullet| {
            let (mark, items): (Element<'a, Message>, _) = match bullet {
                Bullet::Point { items } => {
                    let dot = text("•").size(settings.text_size * 0.75).line_height(LineHeight::Absolute(line.into()));
                    (dot.align_y(Vertical::Center).style(kit::text(Tone::Muted)).into(), items)
                }
                Bullet::Task { items, done } => (container(checkbox(*done).size(settings.text_size)).center_y(line).into(), items),
            };
            row![mark, markdown::view_with(items, inner, self)].spacing(sp(Sp::S)).into()
        }))
        .spacing(settings.spacing * 0.25)
        .padding(pad_x(Sp::S))
        .into()
    }

    fn code_block(&self, settings: Settings, _language: Option<&'a str>, code: &'a str, _lines: &'a [Text]) -> Element<'a, Message> {
        let lines = self.selection.text(vec![span(code_text(code))], settings.code_size, LineHeight::default(), settings.style.code_block_font);
        let bar = scrollable::Scrollbar::default().width(4).scroller_width(4);
        let body = scrollable(container(lines).padding(sp(Sp::M))).direction(scrollable::Direction::Horizontal(bar));
        let index = self.code_index.get();
        self.code_index.set(index + 1);
        let source = self.source;
        // Display-only hard breaks and Iced's streaming tail handling must not alter clipboard code.
        let copy = copy_button_with("Copy code block", move || {
            let mut index = index;
            take_code(markdown::parse(source), &mut index).unwrap_or_else(|| code_text(code))
        });
        container(column![copy, body]).width(Fill).style(kit::container(Surface::Well)).into()
    }
}
