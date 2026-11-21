//! Every setting and configuration value the app reads, edits or displays.
//!
//! - [`app`] is the app's own TOML config with a schema version.
//! - [`models`] edits omp's model settings files and declares the omp ports
//!   (RPC, provider login, file resolution, validation) the harness adapters
//!   implement.
//! - [`projects`] holds per-project settings and the project overview shown in
//!   settings.
//! - [`updates`] holds release, gate and peer standing data shown in settings.
//!
//! This crate depends only on the foundation crates; domain crates and the UI
//! import their settings types from here.

pub mod app;
pub mod models;
pub mod projects;
pub mod updates;
