#pragma once

// Remote hosts between the central config and the Tools panel's `loa::view::panel` view.

#include <string>
#include <string_view>
#include <utility>

#include "loa/settings/app/schema.hpp"
#include "loa/view/panel.hpp"

namespace loa::app::view {

// The Remote hosts entry `alias` as the panel lists it.
[[nodiscard]] ::loa::view::panel::RemoteHost host_to_view(std::string_view alias, const ::loa::settings::app::RemoteHost& entry);
// A host from the panel as its alias and config entry.
[[nodiscard]] std::pair<std::string, ::loa::settings::app::RemoteHost> host_from_view(::loa::view::panel::RemoteHost host);

} // namespace loa::app::view
