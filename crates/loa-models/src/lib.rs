//! Sign in, model listing and model settings through omp and other harnesses.
//!
//! The app has no provider code of its own. [`service::Backend`] is the logic
//! behind the sign in screen, the model picker and the model settings screen.
//! The settings data, file editors and omp ports it drives live in
//! `loa_settings::models`; `loa-harness` implements those ports over a
//! dedicated `omp --mode rpc --no-session` process and omp's own CLI, and the
//! app wires them into a [`service::Backend`] through [`service::Parts`].
//!
//! No function here accepts, stores or returns an API key value; see
//! `loa_settings::models` for how keys are referenced instead.

#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod service;
#[cfg(test)]
mod service_tests;
