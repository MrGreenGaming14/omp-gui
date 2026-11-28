#pragma once

// Command execution results for `remote_bash`.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>

#include "loa/runtime/json.hpp"

namespace loa::remote {

// How a command ended.
enum class ExecEnd {
    // The remote side closed the channel.
    exited,
    timed_out,
    cancelled,
};
[[nodiscard]] std::string_view to_string(ExecEnd end) noexcept;

// Captured result of a remote command; `*_dropped` count bytes past the output limit.
struct ExecOutput {
    std::string stdout_text;
    std::string stderr_text;
    std::optional<std::uint32_t> exit_status;
    std::optional<std::string> exit_signal;
    std::uint64_t stdout_dropped = 0;
    std::uint64_t stderr_dropped = 0;
    ExecEnd end = ExecEnd::exited;

    // The serde form: `stdout`, `stderr`, `exit_status`, `exit_signal`, `stdout_dropped`,
    // `stderr_dropped`, `end`.
    [[nodiscard]] loa::runtime::json::Value to_json() const;
    friend bool operator==(const ExecOutput&, const ExecOutput&) = default;
};

} // namespace loa::remote
