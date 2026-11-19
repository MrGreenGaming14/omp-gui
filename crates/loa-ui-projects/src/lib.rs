//! The project screen in the shape `loa-app` composes: a [`State`] with
//! `update` and `view`, an internal [`Message`] and an [`Event`] the app acts
//! on. The screen shows a [`loa_projects::ProjectOverview`] and never loads
//! or stores anything itself.

pub mod screen;
mod style;

pub use screen::{Event, Message, State};
