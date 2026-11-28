#pragma once

// The `remote` tools on loopback streamable HTTP for harnesses that connect to MCP servers
// themselves. Each session gets its own bearer token, which names the session its calls carry;
// requests without a known token get 401.

#include <expected>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>

#include "loa/contracts/harness.hpp"
#include "loa/contracts/tools.hpp"

namespace loa::runtime {
class Executor;
}

namespace loa::mcp {
class ToolSetMcpHttp;
}

namespace loa::remote {

// The running HTTP server; stops when destroyed.
class RemoteHttp {
  public:
    // Serves `tools` on `127.0.0.1` at a free port, running calls on `exec`.
    [[nodiscard]] static std::expected<RemoteHttp, std::string> start(loa::contracts::ToolSet tools, loa::runtime::Executor& exec);
    RemoteHttp(RemoteHttp&&) noexcept;
    RemoteHttp& operator=(RemoteHttp&&) noexcept;
    ~RemoteHttp();

    // `http://127.0.0.1:<port>/mcp`.
    [[nodiscard]] const std::string& url() const noexcept { return url_; }
    // The `remote` server spec for one session with a new bearer token; older tokens stay valid
    // until `forget`.
    [[nodiscard]] loa::contracts::McpServerSpec session_spec(std::string_view session_id) const;
    // Revokes every token of a session (it ended).
    void forget(std::string_view session_id) const;
    // Stops accepting requests.
    void shutdown();

    struct Tokens {
        std::mutex mu;
        std::map<std::string, std::string> by_token;
    };

  private:
    RemoteHttp(std::unique_ptr<loa::mcp::ToolSetMcpHttp> server, std::shared_ptr<Tokens> tokens, std::string url);

    std::unique_ptr<loa::mcp::ToolSetMcpHttp> server_;
    std::shared_ptr<Tokens> tokens_;
    std::string url_;
};

} // namespace loa::remote
