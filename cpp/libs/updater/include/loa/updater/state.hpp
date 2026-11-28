#pragma once

// Persisted updater decisions in `<root>/state.json`.

#include <cstdint>
#include <map>
#include <string>

#include "loa/settings/updates.hpp"
#include "loa/updater/error.hpp"

namespace loa::updater {

// Everything the updater remembers between runs, keyed by product id. Missing fields read as empty.
struct UpdaterState {
    // Versions the user pinned; no other version is offered.
    std::map<std::string, Version> pins;
    std::map<std::string, loa::settings::HeldUpdate> held;
    // Waited-on versions: neither they nor anything older is offered again.
    std::map<std::string, Version> waits;
    std::map<std::string, std::uint64_t> last_check_unix;
    // Versions cleared for swapping at the next point between sessions.
    std::map<std::string, Version> pending_swap;
    std::map<std::string, loa::settings::GateReport> reports;

    // Reads the state under `root`; a missing file is an empty state.
    [[nodiscard]] static Result<UpdaterState> load(const Path& root);
    [[nodiscard]] Result<> save(const Path& root) const;
    friend bool operator==(const UpdaterState&, const UpdaterState&) = default;
};

[[nodiscard]] loa::runtime::json::Value to_json(const UpdaterState& v);
[[nodiscard]] std::expected<UpdaterState, std::string> updater_state_from_json(const loa::runtime::json::Value& v);

} // namespace loa::updater
