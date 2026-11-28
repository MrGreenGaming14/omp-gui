#pragma once

// A `ToolSet` served over MCP (stdio or streamable HTTP), and the conversions between MCP tool
// results and `ToolOutput` (`to_output` and `text` live in `host_tools.hpp`).

#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/output.hpp"
#include "loa/contracts/tools.hpp"
#include "loa/mcp/error.hpp"
#include "loa/runtime/json.hpp"

namespace loa::runtime {
class Executor;
}

namespace loa::mcp {

namespace detail {
class HttpServer;
}

// An image block from base64 `data`; none when it doesn't decode.
[[nodiscard]] std::optional<contracts::OutputBlock> image(std::string_view data, std::string_view mime);
// The MCP `CallToolResult` wire object for `output`, images base64 encoded.
[[nodiscard]] runtime::json::Value to_result(const contracts::ToolOutput& output);

// Request headers of a streamable HTTP call, as received.
using HttpHeaders = std::vector<std::pair<std::string, std::string>>;
// Admits an HTTP request and names its caller; none answers 401.
using HttpAuth = std::function<std::optional<contracts::CallCtx>(const HttpHeaders&)>;

// A running streamable HTTP endpoint (`POST /mcp`); stops when destroyed.
class ToolSetMcpHttp {
  public:
    ~ToolSetMcpHttp();
    ToolSetMcpHttp(ToolSetMcpHttp&&) noexcept;
    ToolSetMcpHttp& operator=(ToolSetMcpHttp&&) noexcept;
    [[nodiscard]] std::uint16_t port() const noexcept;
    // `http://<host>:<port>/mcp`.
    [[nodiscard]] const std::string& url() const noexcept { return url_; }
    void stop();

  private:
    friend class ToolSetMcp;
    ToolSetMcpHttp(std::unique_ptr<detail::HttpServer> server, std::string url);
    std::unique_ptr<detail::HttpServer> server_;
    std::string url_;
};

// An MCP server offering the tools of a `ToolSet`. Cheap to copy; copies share the tools.
class ToolSetMcp {
  public:
    ToolSetMcp(contracts::ToolSet tools, std::string_view name);

    [[nodiscard]] ToolSetMcp with_instructions(std::string_view text) const;

    [[nodiscard]] const std::string& name() const noexcept { return name_; }
    [[nodiscard]] const std::optional<std::string>& instructions() const noexcept { return instructions_; }

    // Answers one JSON-RPC message; none for notifications and responses. `ctx` names the caller.
    [[nodiscard]] contracts::Awaitable<std::optional<runtime::json::Value>> handle(runtime::json::Value message,
                                                                                  contracts::CallCtx ctx) const;

    // Serves newline-delimited JSON-RPC on `in_fd`/`out_fd` until the client disconnects, running
    // calls on `exec`. Blocks the calling thread. `notifications/cancelled` answers the call
    // with "tool call cancelled" and cancels its coroutine.
    [[nodiscard]] Result<void> serve(runtime::Executor& exec, int in_fd, int out_fd) const;
    // Serves on this process's stdin and stdout.
    [[nodiscard]] Result<void> serve_stdio(runtime::Executor& exec) const;
    // Serves streamable HTTP (JSON responses) on `host:port` (`0` picks a free port).
    [[nodiscard]] Result<ToolSetMcpHttp> serve_http(runtime::Executor& exec, const std::string& host, std::uint16_t port,
                                                    HttpAuth auth = {}) const;

  private:
    std::shared_ptr<const contracts::ToolSet> tools_;
    std::string name_;
    std::optional<std::string> instructions_;
};

} // namespace loa::mcp
