#pragma once

// The in-house MCP client: JSON-RPC 2.0 over a child's stdio (newline-delimited JSON) or over
// streamable HTTP (libcurl). Private to loa::mcp; the manager is its only user.

#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <expected>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <stop_token>
#include <string>
#include <string_view>
#include <vector>

#include "loa/mcp/model.hpp"
#include "loa/mcp/spec.hpp"
#include "loa/runtime/json.hpp"

namespace loa::mcp::detail {

using Clock = std::chrono::steady_clock;

// The MCP protocol version the client asks for.
inline constexpr std::string_view PROTOCOL_VERSION = "2025-06-18";

// When an operation gives up: on a stop request or past the deadline.
struct Cancel {
    std::stop_token stop;
    std::optional<Clock::time_point> deadline;
    [[nodiscard]] bool expired() const { return stop.stop_requested() || (deadline && Clock::now() >= *deadline); }
};

// Where a transport hands what it receives.
struct Sink {
    std::function<void(std::string_view)> message;
    std::function<void()> closed;
};

class Transport {
  public:
    virtual ~Transport() = default;
    // Sends one JSON-RPC message. A synchronous transport (HTTP) hands the reply to the sink before returning.
    [[nodiscard]] virtual std::expected<void, std::string> send(const std::string& message, const Cancel& cancel) = 0;
    [[nodiscard]] virtual bool synchronous() const noexcept = 0;
    // Called once the handshake agreed on a protocol version.
    virtual void initialized(std::string /*protocol_version*/) {}
    // Ends the connection (kills a child's process group); idempotent.
    virtual void close() = 0;
    // The last non empty line the child wrote to stderr once the stream closed (waiting up to a second).
    [[nodiscard]] virtual std::optional<std::string> last_line() { return std::nullopt; }
};

// The child's own process group (Windows: job object), so closing also reaches what the server spawned.
[[nodiscard]] std::expected<std::unique_ptr<Transport>, std::string> spawn_stdio(const StdioSpec& spec, Sink sink);
[[nodiscard]] std::unique_ptr<Transport> open_http(const HttpSpec& spec, Sink sink);
// Initializes libcurl once; the error text when it can't.
[[nodiscard]] std::expected<void, std::string> init_http();

// One client connection after its handshake. Thread-safe; calls may come from any thread.
class Connection {
  public:
    enum class Wake { Notification, Closed, Stopped };

    // Starts the server (or opens the HTTP session) and runs the MCP handshake within `timeout`.
    [[nodiscard]] static Result<std::shared_ptr<Connection>> open(const ServerSpec& spec, std::chrono::milliseconds timeout,
                                                                  std::stop_token stop);
    ~Connection();
    Connection(const Connection&) = delete;
    Connection& operator=(const Connection&) = delete;

    // A request's result; the error is the service failure text.
    [[nodiscard]] std::expected<runtime::json::Value, std::string> request(std::string_view method, runtime::json::Value params,
                                                                          const Cancel& cancel = {});
    void notify(std::string_view method, std::optional<runtime::json::Value> params = std::nullopt);

    [[nodiscard]] std::expected<std::vector<model::Tool>, std::string> list_all_tools(const Cancel& cancel = {});
    [[nodiscard]] std::expected<std::vector<model::Resource>, std::string> list_all_resources();
    [[nodiscard]] std::expected<model::CallToolResult, std::string> call_tool(std::string_view name,
                                                                              std::optional<model::JsonObject> arguments);
    [[nodiscard]] std::expected<std::vector<model::ResourceContents>, std::string> read_resource(std::string_view uri);

    // What the server declared in its `initialize` result.
    [[nodiscard]] const runtime::json::Value& server_capabilities() const noexcept { return capabilities_; }

    // Blocks until a server notification arrives (its method in `method`), the connection closes or `stop`.
    [[nodiscard]] Wake wait(std::stop_token stop, std::string& method);
    [[nodiscard]] bool closed() const;
    [[nodiscard]] std::optional<std::string> last_line();
    void close();

  private:
    struct Pending {
        bool done = false;
        std::expected<runtime::json::Value, std::string> result;
    };

    Connection() = default;
    void deliver(std::string_view text);
    void dispatch(const runtime::json::Value& message);
    void on_closed();

    std::unique_ptr<Transport> transport_;
    runtime::json::Value capabilities_;
    mutable std::mutex mutex_;
    std::condition_variable_any changed_;
    std::int64_t next_id_ = 1;
    std::map<std::int64_t, std::shared_ptr<Pending>> pending_;
    std::deque<std::string> notifications_;
    bool closed_ = false;
};

// The last non empty line of `text`.
[[nodiscard]] std::optional<std::string> last_nonempty_line(std::string_view text);

} // namespace loa::mcp::detail
