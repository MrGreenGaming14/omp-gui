//! The frame of the models screen: a heading that switches between the sign
//! in, model and settings pages, a close button, and the page below.

use iced::widget::{button, center, column, container, row, space, text};
use iced::{Element, Fill};
use loa_ui_kit::icons::Icon;

use crate::style;

/// The pages of the models screen.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    /// Providers and harness logins ([`crate::signin`]).
    SignIn,
    /// The session's model ([`crate::picker`]).
    Model,
    /// omp's model settings ([`crate::settings`]).
    Settings,
}

impl Page {
    pub const ALL: [Page; 3] = [Page::SignIn, Page::Model, Page::Settings];

    pub fn title(self) -> &'static str {
        match self {
            Page::SignIn => "Sign in",
            Page::Model => "Model",
            Page::Settings => "Settings",
        }
    }
}

/// The screen showing `page`: the page switch and close button, then `body`.
pub fn frame<'a, M: Clone + 'a>(
    page: Page,
    body: Element<'a, M>,
    select: impl Fn(Page) -> M,
    close: M,
) -> Element<'a, M> {
    let mut header = row![text("Models").size(style::BODY), space::horizontal().width(12)].spacing(4).align_y(iced::Center);
    for each in Page::ALL {
        let style = if each == page { style::selected } else { style::flat };
        header = header.push(button(text(each.title()).size(style::SMALL)).padding([4.0, 10.0]).style(style).on_press(select(each)));
    }
    header = header.push(space::horizontal()).push(style::icon_button(Icon::X).on_press(close));
    column![container(header).padding([8.0, 12.0]), style::divider(), container(body).width(Fill).height(Fill)].into()
}

/// A centered muted line standing in for a page that is loading or could
/// not be read.
pub fn status<'a, M: 'a>(message: impl text::IntoFragment<'a>) -> Element<'a, M> {
    center(text(message).size(style::NOTE).style(style::muted)).into()
}
