#pragma once

// Claude Code's SDK MCP server sends host tool JSON-RPC through mcp_message
// control requests on the existing stdio connection.

#include <memory>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/harness/kit/rpc.hpp"

namespace loa::harness::claude::mcp {

inline constexpr std::string_view HOST_SERVER = "loa-host";

// Notifications still receive a JSON-RPC result: the control protocol needs an ack.
[[nodiscard]] loa::contracts::Awaitable<kit::Json> handle(
    const std::vector<loa::contracts::HostToolSpec>& tools,
    const std::shared_ptr<const loa::contracts::HostTools>& handler,
    std::string_view session_id, const kit::Json& message);

[[nodiscard]] kit::Json error(const kit::Json& message, std::string text);

} // namespace loa::harness::claude::mcp
