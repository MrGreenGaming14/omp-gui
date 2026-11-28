#pragma once

// Whether a harness can run on this machine, for the harness picker.

#include <chrono>
#include <filesystem>
#include <string>
#include <vector>
#include <optional>

namespace loa::contracts {

enum class AvailabilityStatus {
    Ready,
    NotInstalled,
    // A server harness that doesn't answer.
    Unreachable,
    Incompatible,
    // The executable didn't run or answer.
    Failed,
    Unknown,
};

// What `Adapter::availability` found.
struct Availability {
    // How long a `--version` probe of the executable may take.
    static constexpr std::chrono::seconds PROBE_TIMEOUT{10};

    AvailabilityStatus status = AvailabilityStatus::Unknown;
    // Short picker note: the reason when not ready, else the version or address.
    std::optional<std::string> detail;
    // The executable found, for `SessionConfig::executable`.
    std::optional<std::filesystem::path> executable;
    std::optional<std::string> version;

    [[nodiscard]] static Availability ready(std::optional<std::filesystem::path> executable, std::optional<std::string> version) {
        return {AvailabilityStatus::Ready, version, std::move(executable), version};
    }

    [[nodiscard]] static Availability unavailable(AvailabilityStatus status, std::string reason) {
        return {status, std::move(reason), std::nullopt, std::nullopt};
    }

    [[nodiscard]] bool is_ready() const noexcept { return status == AvailabilityStatus::Ready; }

    // Classifies a `loa::runtime::exec` probe of `program version_args`: `NotInstalled` when
    // it isn't on `PATH`, `Failed` (with the executable) when it didn't answer within
    // `PROBE_TIMEOUT`, else ready with the parsed version.
    [[nodiscard]] static Availability probe(const std::filesystem::path& program, const std::vector<std::string>& version_args);

    friend bool operator==(const Availability&, const Availability&) = default;
};

} // namespace loa::contracts
