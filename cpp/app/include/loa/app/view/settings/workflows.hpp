#pragma once

// Projections between stored workflows and their editable views.

#include "loa/settings/models/error.hpp"
#include "loa/settings/models/workflows.hpp"
#include "loa/view/settings_workflows.hpp"

namespace loa::app::view::settings {

[[nodiscard]] ::loa::view::settings::workflows::Workflow to_view(const ::loa::settings::models::workflows::Workflow& workflow);
[[nodiscard]] ::loa::settings::models::Result<::loa::settings::models::workflows::Workflow> from_view(::loa::view::settings::workflows::Workflow workflow);

} // namespace loa::app::view::settings
