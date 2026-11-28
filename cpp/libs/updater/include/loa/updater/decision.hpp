#pragma once

// Turns a gate report into swap or hold, and applies the user's choice for a held update.

#include <cstdint>
#include <optional>
#include <string_view>

#include "loa/settings/updates.hpp"
#include "loa/updater/state.hpp"

namespace loa::updater::decision {

// Records `report`: a clean one becomes a pending swap, a harmful one a hold. Returns whether it was clean.
bool record(UpdaterState& state, const loa::settings::GateReport& report, std::uint64_t now_unix);

// Applies `choice` to the update held for `product`; returns the version to swap to now.
std::optional<Version> apply_choice(UpdaterState& state, std::string_view product, const Version& current, loa::settings::HoldChoice choice);

// Whether `version` may be offered: not against another pinned version, nor at or below a wait.
[[nodiscard]] bool offer_allowed(const UpdaterState& state, std::string_view product, const Version& version);

} // namespace loa::updater::decision
