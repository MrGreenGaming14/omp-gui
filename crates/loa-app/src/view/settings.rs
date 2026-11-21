//! Projections between the settings domain (`loa_settings`, the omp login contract) and
//! [`loa_view::settings`]: models, projects, updates and omp agents ([`agents`]) definitions. Peers
//! are projected by `areas::peers::adapter`, which owns the node.

pub mod agents;
pub mod models;
pub mod projects;
pub mod updates;
