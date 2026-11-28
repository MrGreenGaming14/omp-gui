#pragma once

// The updater's products and peer standings as `loa::view::settings::updates` views, and the
// answer to a held update back as the updater's choice.

#include "loa/settings/updates.hpp"
#include "loa/view/settings_updates.hpp"

namespace loa::app::view::settings {

[[nodiscard]] ::loa::view::settings::updates::Product product_to_view(const ::loa::settings::ProductStatus& p);
[[nodiscard]] ::loa::view::settings::updates::PeerStanding peer_to_view(const ::loa::settings::PeerStanding& p);
[[nodiscard]] ::loa::settings::HoldChoice hold_choice_from_view(::loa::view::settings::updates::HoldChoice choice) noexcept;

} // namespace loa::app::view::settings
