//! Projections between the settings domain (`loa_settings`, the omp login contract) and
//! [`loa_view::settings`]. Peers are projected by `areas::peers::adapter`, which owns the node.

pub mod models;
pub mod projects;
pub mod updates;
