#pragma once

// The Remote hosts of the app's central config (`RemoteHost`) as connectable `Host`s, resolved
// through an `SshResolver` one hop at a time, and `~/.ssh/config` aliases as new entries.

#include <cstddef>
#include <cstdint>
#include <map>
#include <optional>
#include <string>
#include <string_view>
#include <tuple>
#include <utility>
#include <vector>

#include "loa/remote/error.hpp"
#include "loa/remote/hosts.hpp"
#include "loa/remote/limits.hpp"
#include "loa/remote/resolve.hpp"
#include "loa/settings/app/schema.hpp"

namespace loa::remote {

using RemoteHost = loa::settings::app::RemoteHost;

// The longest ProxyJump chain followed.
inline constexpr std::size_t MAX_JUMPS = 16;

// The limits a config entry sets; `auto_approve` covers writes and commands.
[[nodiscard]] HostLimits limits(const RemoteHost& entry);
// Resolves one entry like `ssh -l USER -p PORT -J JUMP HOST`: the entry's own values win.
[[nodiscard]] Result<Host> host(std::string_view name, const RemoteHost& entry, const SshResolver& resolver);
// Every entry as a `Host`, and the names that failed to resolve.
[[nodiscard]] std::pair<std::vector<Host>, std::vector<std::pair<std::string, Error>>> load(
    const std::map<std::string, RemoteHost>& entries, const SshResolver& resolver);
// A new entry for an ssh config alias; limits start closed (read only, no paths, no commands).
[[nodiscard]] RemoteHost entry_for_alias(std::string_view alias);
// Adds every alias `entries` doesn't have yet; returns the names added.
std::vector<std::string> import_aliases(std::map<std::string, RemoteHost>& entries, const SshResolver& resolver);
// `[ssh://][user@]host[:port]`, IPv6 hosts in brackets.
[[nodiscard]] std::expected<std::tuple<std::string, std::optional<std::string>, std::optional<std::uint16_t>>, std::string>
parse_jump(std::string_view spec);
// The hop for `alias` as resolved: `IdentityAgent` mapped to an `AgentSocket` (`~/` expanded).
[[nodiscard]] Endpoint endpoint(std::string_view alias, ResolvedHost resolved);

} // namespace loa::remote
