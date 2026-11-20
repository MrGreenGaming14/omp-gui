//! The project screens in the shape `loa-app` composes: each a `State` with
//! `update` and `view`, an internal `Message` and an `Event` the app acts
//! on. [`screen`] shows a [`loa_projects::ProjectOverview`]; [`create`] is
//! the New project form. Neither loads or stores anything itself.

pub mod create;
pub mod screen;
mod style;

pub use screen::{Event, Message, State};
