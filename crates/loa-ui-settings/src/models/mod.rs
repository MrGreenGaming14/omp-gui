//! The models screen: sign in ([`signin`]), the session's model ([`picker`]) and omp's model
//! settings ([`settings`]) under one page switch, over the view types of
//! [`loa_view::settings::models`].

pub mod picker;
pub mod settings;
pub mod signin;

use iced::widget::{Space, center, column, container, row, rule};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::style::{self, Tone};
use loa_ui_kit::theme::{SpaceRole, TextRole, sp};
use loa_ui_kit::widgets::{Tab, icon_button, label, tab_strip};
use loa_view::theme::Icon;

loa_util::str_enum! {
    /// The pages of the models screen.
    pub Page { SignIn => "Sign in", Model => "Model", Settings => "Settings", Workflows => "Workflows" }
}

/// The screen showing `page`: the page switch and close button, then `body`.
pub fn frame<'a, M: Clone + 'a>(page: Page, body: Element<'a, M>, select: impl Fn(Page) -> M, close: M) -> Element<'a, M> {
    let tabs = tab_strip(Page::ALL.iter().map(|&p| Tab { label: p.as_str(), icon: None, active: p == page, on: select(p), id: None, trailing: None }));
    let head = row![label("Models", TextRole::Heading, Tone::Normal), tabs, Space::new().width(Fill), icon_button(Icon::X, "Close", Some(close))];
    let head = container(head.spacing(sp(SpaceRole::M)).align_y(Alignment::Center)).padding([sp(SpaceRole::S), sp(SpaceRole::M)]);
    column![head, rule::horizontal(1).style(style::rule), container(body).width(Fill).height(Fill)].into()
}

/// A centered muted line standing in for a page that is loading or could not be read.
pub fn status<'a, M: 'a>(message: &'a str) -> Element<'a, M> { center(label(message, TextRole::Label, Tone::Muted)).into() }
