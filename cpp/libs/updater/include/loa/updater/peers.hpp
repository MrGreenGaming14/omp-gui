#pragma once

// Classifies peers by app version and shared protocol labels for the Peers list.
//
// Nodes talk over exact ALPN labels (`omp/<family>/<n>`): a family is shared only when
// both sides announce the same label.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/settings/updates.hpp"
#include "loa/updater/error.hpp"

namespace loa::updater::peers {

// Splits an `omp/<family>/<n>` label into family and version.
[[nodiscard]] std::optional<std::pair<std::string_view, std::uint32_t>> parse_alpn(std::string_view label);

// Classifies `peers`; sorted with peers that can't talk first, then the most behind, then by node.
[[nodiscard]] std::vector<loa::settings::PeerStanding> classify(const Version& local_version, const std::vector<std::string>& local_alpns,
                                                                const std::vector<loa::settings::PeerInfo>& peers);

} // namespace loa::updater::peers
