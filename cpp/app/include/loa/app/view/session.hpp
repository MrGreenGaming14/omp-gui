#pragma once

// The session screen's projections: the unattended policy between the central config and
// `loa::view::session::decisions`.

#include "loa/settings/app/schema.hpp"
#include "loa/view/session.hpp"

namespace loa::app::view {

[[nodiscard]] ::loa::view::session::decisions::UnattendedView unattended_to_view(const ::loa::settings::app::UnattendedSettings& settings);
[[nodiscard]] ::loa::settings::app::UnattendedSettings unattended_from_view(::loa::view::session::decisions::UnattendedView view);

} // namespace loa::app::view
