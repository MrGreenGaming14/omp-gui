#pragma once

// The built-in surface servers as streamable HTTP MCP servers on loopback
// (`http://127.0.0.1:<port>/<server>`), for harnesses that connect to MCP
// servers themselves. Each declares its surfaces as `surface://` resources and
// serves the surface tools; requests must carry the bearer token. Port of
// `loa-surfaces/src/hub/endpoint.rs`.

#include <expected>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/runtime/error.hpp"

namespace loa::surfaces::hub {

class SurfaceTools;

// MCP server names are `surface-<server>`, e.g. `surface-browser`.
inline constexpr std::string_view SERVER_PREFIX = "surface-";

// The running loopback MCP endpoint. Destroying it stops the server.
class McpEndpoint {
  public:
    // Serves `tools` on `127.0.0.1` at a free port; tool calls run on the calling coroutine's
    // executor, which must have threads other than the HTTP server's.
    [[nodiscard]] static loa::contracts::Awaitable<std::expected<McpEndpoint, loa::Error>> start(std::shared_ptr<SurfaceTools> tools);

    ~McpEndpoint();
    McpEndpoint(McpEndpoint&&) noexcept;
    McpEndpoint& operator=(McpEndpoint&&) noexcept;

    // `http://127.0.0.1:<port>/<server>` of a built-in server.
    [[nodiscard]] std::string url(std::string_view server) const;
    // The bearer token every request must carry.
    [[nodiscard]] const std::string& token() const noexcept;
    // The built-in servers as MCP servers for a session config.
    [[nodiscard]] std::vector<loa::contracts::McpServerSpec> specs() const;

    struct Impl;

  private:
    explicit McpEndpoint(std::unique_ptr<Impl> impl);
    std::unique_ptr<Impl> impl_;
};

} // namespace loa::surfaces::hub
