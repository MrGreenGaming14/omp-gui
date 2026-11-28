#pragma once

// The harnesses the runtime can route sessions to (port of `loa_runtime::harnesses`).

#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/account.hpp"
#include "loa/contracts/availability.hpp"
#include "loa/contracts/harness.hpp"

namespace loa::app_runtime {

// One harness as the picker and the Harnesses tab list it.
struct Harness {
    // Harness id, e.g. `omp`; the id its adapter registers under.
    std::string id;
    std::string label;
    // It can be picked: its probe found it ready.
    bool available = false;
    // Note beside it: why it can't run, or what the probe found.
    std::optional<std::string> detail;
    // Executable passed in `SessionConfig::executable`; `nullopt` lets the adapter run its default.
    std::optional<std::filesystem::path> executable;
    // The executable `[harness.executables]` names instead of the default.
    std::optional<std::filesystem::path> configured;
    // What the last probe found; `nullopt` until it answers.
    std::optional<contracts::Availability> availability;
    contracts::HarnessTraits traits;
    // What runs when nothing is configured (the server URL of a server harness).
    std::string_view program;
};

// Every harness the runtime knows, with their adapters.
struct Harnesses {
    // Every adapter, runnable or not; `runnable` picks the ones that can start.
    contracts::Registry registry;
    std::vector<Harness> list;

    // Registers `adapter` and lists it as `harness`, replacing the entry with its id, until
    // `set_availability` reports a probe. `harness.id` is the adapter's `harness()`.
    void add(std::shared_ptr<contracts::Adapter> adapter, Harness harness);

    // Records what harness `id`'s probe found: whether it can be picked, the note beside it
    // and the executable sessions run.
    void set_availability(std::string_view id, contracts::Availability availability);

    // The adapter of harness `id` when it can start sessions here.
    [[nodiscard]] std::shared_ptr<contracts::Adapter> runnable(std::string_view id) const;

    [[nodiscard]] const Harness* get(std::string_view id) const;

    // What the app asks of harness `id`; the defaults for one it doesn't know.
    [[nodiscard]] contracts::HarnessTraits traits(std::string_view id) const;

    // Label of the harness with this id, or the id itself.
    [[nodiscard]] std::string_view label(std::string_view id) const;

    // The harness whose model catalog and roles the models area shows (omp); empty when none
    // is compiled in.
    [[nodiscard]] std::string_view roles_id() const;

    // The first harness with model roles (omp) when it can run, else the first that can.
    [[nodiscard]] std::optional<std::string> default_id() const;
};

} // namespace loa::app_runtime
