//! Assistant markdown in the transcript's look: roomy body text, inline code
//! as small chips, muted bullets, and an optional muted marker ("[interrupted]")
//! at the end of the last paragraph.

use crate::Message;
use iced::{Background, Color, Font, Padding, border};
use loa_ui_kit::style::{self as kit, Surface, Tone};
use loa_ui_kit::theme::{ColorRole as C, RadiusRole, SpaceRole as Sp, TextRole as T, TokenSet, generation, sp};
use iced::alignment::Vertical;
use iced::widget::markdown::{self, Bullet, Item, Settings, Text};
use iced::widget::text::LineHeight;
use iced::widget::{checkbox, column, container, rich_text, row, scrollable, span, text};
use iced::{Element, Fill};

/// Line height of body text, relative to its size.
pub(crate) const LINE_HEIGHT: f32 = 1.6;

/// Settings for widgets that take colours rather than style functions, built from one token
/// generation; the transcript rebuilds it when the generation changes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Look {
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

/// Renders `items`, ending the last paragraph with `marker` when given.
/// Returns whether the marker found a paragraph to end; when it did not, the
/// caller shows it on its own.
pub(crate) fn view<'a>(
    items: &'a [Item],
    look: Look,
    marker: Option<&'static str>,
) -> (Element<'a, Message>, bool) {
    let tail = marker.and_then(|_| last_text(items));
    let viewer = Prose { look, tail, marker };
    let placed = tail.is_some();
    (markdown::view_with(items, look.markdown, &viewer), placed)
}

/// The text of the paragraph that ends `items`, if they end in one.
fn last_text(items: &[Item]) -> Option<&Text> {
    match items.last()? {
        Item::Paragraph(text) => Some(text),
        Item::List { bullets, .. } => match bullets.last()? {
            Bullet::Point { items } | Bullet::Task { items, .. } => last_text(items),
        },
        Item::Quote(items) => last_text(items),
        _ => None,
    }
}

struct Prose<'a> {
    look: Look,
    tail: Option<&'a Text>,
    marker: Option<&'static str>,
}

fn line_height(settings: &Settings) -> f32 {
    settings.text_size.0 * LINE_HEIGHT
}

impl<'a> markdown::Viewer<'a, Message> for Prose<'a> {
    fn on_link_click(url: markdown::Uri) -> Message {
        Message::Link(url)
    }

    fn paragraph(&self, settings: Settings, text: &Text) -> Element<'a, Message> {
        let spans = text.spans(settings.style);
        let marker = self
            .marker
            .filter(|_| self.tail.is_some_and(|tail| std::ptr::eq(tail, text)));
        // Only inline code (the highlighted spans) and the marker need
        // spans of their own; plain paragraphs reuse the parser's cache.
        let body = if marker.is_none() && spans.iter().all(|s| s.highlight.is_none()) {
            rich_text(spans)
        } else {
            let mut all: Vec<_> = spans
                .iter()
                .map(|s| match s.highlight {
                    Some(_) => s.clone().size(self.look.markdown.code_size),
                    None => s.clone(),
                })
                .collect();
            if let Some(marker) = marker {
                all.push(span(" "));
                all.push(span(marker).color(self.look.muted));
            }
            rich_text(all)
        };
        body.size(settings.text_size)
            .line_height(LINE_HEIGHT)
            .on_link_click(Self::on_link_click)
            .into()
    }

    fn unordered_list(&self, settings: Settings, bullets: &'a [Bullet]) -> Element<'a, Message> {
        let line = line_height(&settings);
        let inner = Settings {
            spacing: settings.spacing * 0.5,
            ..settings
        };
        column(bullets.iter().map(|bullet| {
            let (mark, items): (Element<'a, Message>, _) = match bullet {
                Bullet::Point { items } => (
                    text("•")
                        .size(settings.text_size * 0.75)
                        .line_height(LineHeight::Absolute(line.into()))
                        .align_y(Vertical::Center)
                        .style(kit::text(Tone::Muted))
                        .into(),
                    items,
                ),
                Bullet::Task { items, done } => (
                    container(checkbox(*done).size(settings.text_size))
                        .center_y(line)
                        .into(),
                    items,
                ),
            };
            row![mark, markdown::view_with(items, inner, self)]
                .spacing(sp(Sp::S))
                .into()
        }))
        .spacing(settings.spacing * 0.25)
        .padding(Padding::ZERO.left(sp(Sp::S)).right(sp(Sp::S)))
        .into()
    }

    fn code_block(
        &self,
        settings: Settings,
        _language: Option<&'a str>,
        _code: &'a str,
        lines: &'a [Text],
    ) -> Element<'a, Message> {
        let lines = column(lines.iter().map(|line| {
            rich_text(line.spans(settings.style))
                .on_link_click(Self::on_link_click)
                .font(settings.style.code_block_font)
                .size(settings.code_size)
                .into()
        }));
        let scroller = scrollable::Scrollbar::default().width(4).scroller_width(4);
        container(
            scrollable(container(lines).padding(sp(Sp::M)))
                .direction(scrollable::Direction::Horizontal(scroller)),
        )
        .width(Fill)
        .style(kit::container(Surface::Well))
        .into()
    }
}
