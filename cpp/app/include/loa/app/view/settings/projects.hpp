#pragma once

// A project's settings and overview as `loa::view::settings::projects` views, and edited settings
// back as domain values.

#include "loa/settings/projects.hpp"
#include "loa/view/settings_projects.hpp"

namespace loa::app::view::settings {

[[nodiscard]] ::loa::view::settings::projects::ProjectSettings settings_to_view(const ::loa::settings::ProjectSettings& s);
[[nodiscard]] ::loa::settings::ProjectSettings settings_from_view(::loa::view::settings::projects::ProjectSettings s);
[[nodiscard]] ::loa::view::settings::projects::ProjectOverview overview_to_view(const ::loa::settings::ProjectOverview& o);

} // namespace loa::app::view::settings
