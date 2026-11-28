#pragma once

// Per host limits: allowed paths, read only, commands allowed. All checks here run before a byte
// is sent; the symlink check that needs the remote file system lives in the service.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/remote/audit.hpp"

namespace loa::remote {

// What the agent may do on one host.
struct HostLimits {
    // Absolute path prefixes files must sit under; empty allows no file access, `/` every path.
    std::vector<std::string> allowed_paths;
    // Refuse writes, edits and commands.
    bool read_only = false;
    // Whether `remote_bash` may run at all.
    bool commands = true;
    bool auto_approve_writes = false;
    bool auto_approve_commands = false;
    // Cap on captured stdout and on captured stderr, each.
    std::size_t max_output_bytes = 256 * 1024;
    std::uint64_t max_file_bytes = 8 * 1024 * 1024;
    std::uint64_t default_timeout_secs = 120;
    std::uint64_t max_timeout_secs = 3600;

    // Refuses actions the host does not allow at all.
    [[nodiscard]] std::expected<void, std::string> check_action(Action action) const;
    // Refuses a normalized path outside the allowed prefixes.
    [[nodiscard]] std::expected<void, std::string> check_path(std::string_view normalized) const;
    // Whether `action` needs the approval port.
    [[nodiscard]] bool needs_approval(Action action) const noexcept;
    // The allowed prefixes, normalized; relative ones are dropped so they never allow anything.
    [[nodiscard]] std::vector<std::string> prefixes() const;
    // Whether `/` is allowed, so no path (or symlink) can escape.
    [[nodiscard]] bool allows_every_path() const;

    friend bool operator==(const HostLimits&, const HostLimits&) = default;
};

// Lexically normalizes an absolute POSIX path (`//`, `.`, `..` never above `/`); refuses relative paths and NUL.
[[nodiscard]] std::expected<std::string, std::string> normalize(std::string_view path);
// Whether normalized `path` is `prefix` or below it.
[[nodiscard]] bool within(std::string_view path, std::string_view prefix) noexcept;
// The directory and the last component of a normalized path.
[[nodiscard]] std::pair<std::string_view, std::string_view> split(std::string_view path) noexcept;

} // namespace loa::remote
