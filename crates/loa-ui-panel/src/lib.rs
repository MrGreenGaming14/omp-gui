//! Right panel tabs: diff, files, session tree, agents.
//!
//! Each tab is one component in the shape every UI crate uses: a `State`
//! with `update` and `view`, an internal `Message` and an `Event` the app acts
//! on.
//!
//! - [`diff`]: unified and side by side diffs of one or many files
//! - [`files`]: lazy file tree over a [`files::FileSystem`] port
//! - [`session_tree`]: fork and rewind tree of a session family
//! - [`agents`]: live subagents and background jobs from `<job>` elements

pub mod agents;
pub mod diff;
pub mod files;
pub mod session_tree;

use iced::Theme;
use iced::widget::container;
use loa_ui_kit::theme::{AlphaRole, ColorRole, SpaceRole as Sp, TextRole as T, for_theme, sp, text_size as ts};

/// Height of one diff row: a mono line. Rows are uniform so lists virtualize by arithmetic.
pub(crate) fn row_height() -> f32 { ts(T::Mono) + 2.0 * sp(Sp::Xs) }

/// Height of one row in the files, session tree and agents lists.
pub(crate) fn list_row_height() -> f32 { 2.0 * ts(T::Body) }

/// Indentation per tree level.
pub(crate) fn indent() -> f32 { sp(Sp::M) }

/// A fill of `role` at the tint alpha: the changed part inside a diff line.
pub(crate) fn tint(role: ColorRole) -> impl Fn(&Theme) -> container::Style {
    move |theme| { let t = for_theme(theme); container::Style::default().background(t.color(role).scale_alpha(t.alpha(AlphaRole::Tint))) }
}
