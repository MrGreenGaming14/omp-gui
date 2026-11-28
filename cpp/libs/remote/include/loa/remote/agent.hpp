#pragma once

// Authentication through `ssh-agent` only: every identity the agent offers is tried (FIDO2 and
// certificates included) and the agent signs, so no private key enters this process.

#include <filesystem>
#include <optional>
#include <string_view>
#include <vector>

#include "loa/remote/error.hpp"
#include "loa/remote/hosts.hpp"
#include "loa/remote/known_hosts.hpp"

namespace loa::remote {

// The agent socket for a hop: `IdentityAgent` when set, else the app's default.
[[nodiscard]] Result<std::filesystem::path> socket_for(const AgentSocket& agent, const std::optional<std::filesystem::path>& fallback,
                                                       std::string_view host);

// The identities (public keys and certificates) the agent at `socket` offers; on Windows a
// named pipe path.
[[nodiscard]] Result<std::vector<PublicKey>> agent_identities(const std::filesystem::path& socket);

} // namespace loa::remote
