//! Right panel panes, each a component with `update`/`view` and a kit `PanelPane`: [`diff`],
//! [`files`], [`session_tree`], [`agents`] and the [`tools`] page.

pub mod agents;
pub mod diff;
pub mod files;
pub mod session_tree;
pub mod tools;

use iced::Theme;
use iced::widget::{button, container, text};
use loa_ui_kit::style::{self, ButtonKind};
use loa_ui_kit::theme::{AlphaRole, ColorRole, SpaceRole as Sp, TextRole as T, for_theme, sp, text_size as ts};

/// One diff row: a mono line, uniform so lists virtualize by arithmetic.
pub(crate) fn row_height() -> f32 { ts(T::Mono) + 2.0 * sp(Sp::Xs) }

/// One row of the files, session tree and agents lists.
pub(crate) fn list_row_height() -> f32 { 2.0 * ts(T::Body) }

/// Indentation per tree level.
pub(crate) fn indent() -> f32 { sp(Sp::M) }

/// `role` at the tint alpha: the changed part inside a diff line.
pub(crate) fn tint(role: ColorRole) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = for_theme(theme);
        container::Style::default().background(t.color(role).scale_alpha(t.alpha(AlphaRole::Tint)))
    }
}

/// A small ghost button with caption text.
pub(crate) fn action<'a, M: Clone + 'a>(label: impl text::IntoFragment<'a>, on: M) -> button::Button<'a, M> {
    button(text(label).size(ts(T::Caption))).padding([sp(Sp::Xs), sp(Sp::S)]).style(style::button(ButtonKind::Ghost)).on_press(on)
}
