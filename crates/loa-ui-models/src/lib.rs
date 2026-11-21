//! Sign in, model picker and model settings screens.
//!
//! Every screen is a component in the shape `loa-app` composes: a `State`
//! with `update` and `view`, an internal `Message` and an `Event` the app acts
//! on. Screens take plain structs defined here and never call omp themselves.
//!
//! - [`chip`]: the composer chip showing the session's role and model
//! - [`session`]: reading the session's model from a DOM and the drafts that
//!   record a model change on it
//! - [`picker`]: the searchable model picker popup
//! - [`signin`]: provider list with the sign in flow per provider and harness
//! - [`settings`]: model roles, fallbacks, enabled models, provider order,
//!   overrides and custom providers
//! - [`screen`]: the frame switching between those screens' pages

#[cfg(test)]
mod backend_tests;
pub mod chip;
pub mod picker;
pub mod session;
pub mod screen;
pub mod settings;
pub mod signin;
mod style;

pub use loa_contracts::harness::selector::{ModelInfo, ModelRef, RoleAssignment};
pub use session::{SessionModel, SessionModelChange, model_change_drafts};

/// Errors of this crate's non-UI functions.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The DOM has no `<session>` root to record a model change on.
    #[error("the DOM has no session to record the model change on")]
    NoSession,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
