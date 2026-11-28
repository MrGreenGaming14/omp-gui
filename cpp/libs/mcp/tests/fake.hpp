#pragma once

// The fake MCP server the client tests run against: as a stdio child (`loa_mcp_fake_server`)
// and behind an in-process streamable HTTP endpoint.

#include <atomic>
#include <cstdint>
#include <functional>
#include <optional>
#include <string>
#include <vector>

#include "loa/runtime/json.hpp"

namespace fake {

// What the fake's `vault_code` tool returns; a model can't guess it.
inline constexpr const char* VAULT_CODE = "vault code 7731-QZ";
// A 1x1 PNG the fake's `picture` tool returns.
inline const std::vector<std::uint8_t> PNG = {
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d,
    0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
};

class Fake {
  public:
    // Handles one JSON-RPC message; `notify` sends server notifications ahead of the reply.
    // None for notifications.
    std::optional<loa::runtime::json::Value> handle(const loa::runtime::json::Value& message,
                                                    const std::function<void(const loa::runtime::json::Value&)>& notify);

  private:
    std::atomic<bool> grown_{false};
};

} // namespace fake
