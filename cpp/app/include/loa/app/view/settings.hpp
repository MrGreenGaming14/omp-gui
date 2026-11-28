#pragma once

// Projections between the settings domain (`loa::settings`, the omp login contract) and
// `loa::view::settings`: models, projects, updates, omp agents and workflows. Peers are projected
// by the peers area adapter, which owns the node. Port of `loa-app/src/view/settings.rs`.

#include "loa/app/view/settings/agents.hpp"
#include "loa/app/view/settings/models.hpp"
#include "loa/app/view/settings/projects.hpp"
#include "loa/app/view/settings/updates.hpp"
#include "loa/app/view/settings/workflows.hpp"
