//! The start screen: what the center shows with no session open. A heading
//! naming the folder, the composer in the middle (sending starts a session
//! there), and the folder's recent sessions.

use iced::widget::{Column, button, column, container, row, rule, space, text};
use iced::{Alignment, Element, Fill, Length, Padding};
use loa_ui_shell::theme::style;

use super::{App, COMPOSER_WIDTH, Message, workspace};

/// How many recent sessions the start screen lists.
const RECENTS: usize = 4;

/// Messages of the start screen.
#[derive(Debug, Clone)]
pub enum StartMessage {
    /// A recent session was picked.
    Open(String),
}

impl App {
    pub(super) fn start_view(&self) -> Element<'_, Message> {
        let ctx = &self.ctx;
        let starting = ctx.sessions.starting().is_some();
        let project = ctx.services.project.as_deref();
        // Where new sessions go: the active project, else the folder.
        let place = project.map_or_else(|| workspace(&ctx.services.cwd).path, str::to_owned);

        let heading = column![
            text("What should we work on?").size(28).font(iced::Font { weight: iced::font::Weight::Bold, ..iced::Font::DEFAULT }),
            text(if starting { format!("Starting a session in {place}…") } else { format!("in {place}") }).size(14).style(style::muted),
        ]
        .spacing(6)
        .width(Fill)
        .align_x(Alignment::Center);

        let composer = ctx.composer.view().map(Message::Composer);

        let recents: Vec<_> = ctx
            .shell
            .sidebar()
            .recents()
            .into_iter()
            .filter(|summary| project.is_none_or(|project| summary.project.as_deref() == Some(project)))
            .take(RECENTS)
            .collect();
        let now = loa_core::now_ms();
        let recent: Element<'_, Message> = if recents.is_empty() {
            space().into()
        } else {
            let mut list = Column::new();
            for (index, summary) in recents.into_iter().enumerate() {
                if index > 0 {
                    list = list.push(rule::horizontal(1).style(style::divider));
                }
                let line = row![
                    text(summary.title.as_str()).size(14).width(Fill).wrapping(text::Wrapping::None),
                    text(ago(now, summary.updated)).size(12).style(style::muted),
                ]
                .spacing(12)
                .align_y(Alignment::Center);
                list = list.push(
                    button(line)
                        .width(Fill)
                        .padding([9, 14])
                        .style(style::row)
                        .on_press(Message::Start(StartMessage::Open(summary.id.clone()))),
                );
            }
            column![
                text(if project.is_some() { format!("Recent in {place}") } else { "Recent".to_owned() }).size(12).style(style::muted),
                container(list).style(style::card).padding(1).width(Fill),
            ]
            .spacing(8)
            .into()
        };

        let body = column![heading, space().height(12), composer, space().height(8), recent]
            .spacing(12)
            .width(Fill)
            .max_width(COMPOSER_WIDTH);
        let page = iced::widget::scrollable(
            container(body).center_x(Fill).padding(Padding { top: 0.0, right: 24.0, bottom: 24.0, left: 24.0 }),
        )
        .height(Length::Shrink);
        container(page).center(Fill).into()
    }
}

/// "just now", "5m", "3h", "2d" since `then`, both in Unix milliseconds.
pub fn ago(now: i64, then: i64) -> String {
    let minutes = (now - then).max(0) / 60_000;
    match minutes {
        0 => "just now".to_owned(),
        1..=59 => format!("{minutes}m ago"),
        60..=1439 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    }
}

#[cfg(test)]
mod tests {
    use super::ago;

    #[test]
    fn ages_read_in_the_largest_whole_unit() {
        let now = 10 * 86_400_000;
        assert_eq!(ago(now, now - 20_000), "just now");
        assert_eq!(ago(now, now - 5 * 60_000), "5m ago");
        assert_eq!(ago(now, now - 3 * 3_600_000), "3h ago");
        assert_eq!(ago(now, now - 2 * 86_400_000), "2d ago");
        assert_eq!(ago(now, now + 60_000), "just now");
    }
}
