//! Assistant markdown in the transcript's look: roomy body text, inline code
//! as small chips, muted bullets, and an optional muted marker ("[interrupted]")
//! at the end of the last paragraph.

use crate::Message;
use crate::style::{self, LINE_HEIGHT, Look};
use iced::alignment::Vertical;
use iced::widget::markdown::{self, Bullet, Item, Settings, Text};
use iced::widget::text::LineHeight;
use iced::widget::{checkbox, column, container, rich_text, row, scrollable, span, text};
use iced::{Element, Fill};

/// Inline code sits a little smaller than the body text around it.
const INLINE_CODE_SIZE: f32 = 12.5;

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
                    Some(_) => s.clone().size(INLINE_CODE_SIZE),
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
                        .style(style::muted)
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
                .spacing(8)
                .into()
        }))
        .spacing(settings.spacing * 0.25)
        .padding([0.0, 8.0])
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
            scrollable(container(lines).padding(12))
                .direction(scrollable::Direction::Horizontal(scroller)),
        )
        .width(Fill)
        .style(style::block)
        .into()
    }
}
