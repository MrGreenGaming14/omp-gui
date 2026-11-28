#pragma once

// OpenSSH config: the `Host` alias scan (following `Include`) and `ssh -G` resolution.

#include <cstdint>
#include <expected>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/runtime/error.hpp"

namespace loa::runtime::ssh {

// What `ssh -G` reports for one host.
struct ResolvedHost {
    std::string hostname;
    std::string user;
    std::uint16_t port = 0;
    std::optional<std::string> proxy_jump;
    std::optional<std::string> identity_agent;
    friend bool operator==(const ResolvedHost&, const ResolvedHost&) = default;
};

// Concrete (no wildcard, not negated) `Host` aliases of `config` and its includes (followed
// to depth 16, relative to `config`'s directory), lowercased, deduplicated, in file order.
[[nodiscard]] std::vector<std::string> aliases(const std::filesystem::path& config);

// `*` and `?` glob match, as OpenSSH's `match_pattern`.
[[nodiscard]] bool wildcard_match(std::string_view pattern, std::string_view text) noexcept;

// Resolves `alias` with the `ssh` on `PATH` (`ssh -G -- alias`).
[[nodiscard]] std::expected<ResolvedHost, Error> resolve(std::string_view alias);

// Runs `program -G args.. -- host` and parses its output; a missing program is a clear
// `ErrorCode::not_found`.
[[nodiscard]] std::expected<ResolvedHost, Error> resolve_with(const std::filesystem::path& program,
                                                              const std::vector<std::string>& args, std::string_view host);

// Parses `ssh -G` output (`key value` lines); `none` means unset.
[[nodiscard]] ResolvedHost parse(std::string_view text);

} // namespace loa::runtime::ssh
