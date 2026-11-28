#pragma once

// Running registered servers. Each enabled server has a supervisor thread that keeps it
// connected, restarting it with backoff; calls block the calling thread until the server answers.

#include <chrono>
#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/contracts/surface.hpp"
#include "loa/mcp/error.hpp"
#include "loa/mcp/model.hpp"
#include "loa/mcp/spec.hpp"
#include "loa/mcp/tool_filter.hpp"

namespace loa::mcp {

// Delay between restarts of a server that keeps failing: doubles per failure up to `max`; a
// connection that stayed up `max` resets it.
struct Backoff {
    std::chrono::milliseconds initial{500};
    std::chrono::milliseconds max{30'000};

    [[nodiscard]] std::chrono::milliseconds delay(std::uint32_t attempt) const;
    friend bool operator==(const Backoff&, const Backoff&) = default;
};

struct Options {
    Backoff backoff;
    // Limit for spawning or reaching a server and finishing the handshake.
    std::chrono::milliseconds connect_timeout{30'000};
};

// What the Tools panel shows next to a server.
struct Status {
    enum class Kind : std::uint8_t {
        Starting,
        Running,
        // Disabled, stopped by the user, or shut down.
        Stopped,
        // The last start failed or the server went away; a restart is pending unless stopped.
        Error,
    };
    Kind kind = Kind::Stopped;
    std::string message; // `Error` only

    [[nodiscard]] static Status starting() { return {Kind::Starting, {}}; }
    [[nodiscard]] static Status running() { return {Kind::Running, {}}; }
    [[nodiscard]] static Status stopped() { return {Kind::Stopped, {}}; }
    [[nodiscard]] static Status error(std::string message) { return {Kind::Error, std::move(message)}; }
    friend bool operator==(const Status&, const Status&) = default;
};

// Something the UI should redraw for.
namespace event {
struct Status {
    std::string server;
    mcp::Status status;
    friend bool operator==(const Status&, const Status&) = default;
};
struct ToolsChanged {
    std::string server;
    friend bool operator==(const ToolsChanged&, const ToolsChanged&) = default;
};
struct ResourcesChanged {
    std::string server;
    friend bool operator==(const ResourcesChanged&, const ResourcesChanged&) = default;
};
struct Removed {
    std::string server;
    friend bool operator==(const Removed&, const Removed&) = default;
};
} // namespace event
using Event = std::variant<event::Status, event::ToolsChanged, event::ResourcesChanged, event::Removed>;

struct ToolView {
    model::Tool tool;
    // What the filter passed to `Manager::view` says.
    bool allowed = false;
};

// One server as the Tools panel lists it.
struct ServerView {
    ServerSpec spec;
    Status status;
    std::vector<ToolView> tools;
};

// The `surface://` uri a resource declares, if it is a surface.
[[nodiscard]] std::optional<contracts::surface::SurfaceUri> surface_uri(const model::Resource& resource);

// Events published after `Manager::events` was called (a broadcast receiver holding up to
// 256; older ones are dropped when it lags).
class EventReceiver {
  public:
    struct State;
    explicit EventReceiver(std::shared_ptr<State> state) : state_(std::move(state)) {}
    // The next event, none when `timeout` passes first.
    [[nodiscard]] std::optional<Event> recv(std::chrono::milliseconds timeout);

  private:
    std::shared_ptr<State> state_;
};

// A server's status as it changes (a watch receiver).
class StatusWatch {
  public:
    struct State;
    explicit StatusWatch(std::shared_ptr<State> state) : state_(std::move(state)) {}
    [[nodiscard]] Status current() const;
    // Waits until `wanted` holds for the status; none when `timeout` passes first.
    template <typename F>
    [[nodiscard]] std::optional<Status> wait_for(F&& wanted, std::chrono::milliseconds timeout) const {
        return wait_impl([&](const Status& s) { return static_cast<bool>(wanted(s)); }, timeout);
    }

  private:
    [[nodiscard]] std::optional<Status> wait_impl(const std::function<bool(const Status&)>& wanted, std::chrono::milliseconds timeout) const;
    std::shared_ptr<State> state_;
};

// Runs the registered MCP servers. Copies share the same servers.
class Manager {
  public:
    [[nodiscard]] static Result<Manager> make(Options options = {});

    [[nodiscard]] EventReceiver events() const;

    // Registers a server and starts it when it is enabled.
    [[nodiscard]] Result<> add(ServerSpec spec) const;
    // Stops a server and forgets it.
    [[nodiscard]] Result<ServerSpec> remove(std::string_view name) const;
    // Swaps the server registered as `name` (or none) for `spec`, which may carry a new name.
    [[nodiscard]] Result<> replace(std::string_view name, ServerSpec spec) const;
    // Starts a stopped server (and marks it enabled in the running set).
    [[nodiscard]] Result<> start(std::string_view name) const;
    // Stops a server and waits until its child has exited.
    [[nodiscard]] Result<> stop(std::string_view name) const;
    // Stops and starts a server now, skipping any pending backoff.
    [[nodiscard]] Result<> restart(std::string_view name) const;
    // Stops every server and waits for their children to exit.
    void shutdown() const;

    // The registered specs, sorted by name.
    [[nodiscard]] std::vector<ServerSpec> specs() const;
    [[nodiscard]] std::optional<Status> status(std::string_view name) const;
    [[nodiscard]] std::optional<StatusWatch> watch(std::string_view name) const;

    // Every server with its status and tools, each marked with what `filter` allows.
    [[nodiscard]] std::vector<ServerView> view(const ToolFilter& filter) const;
    // The tools of a running server that `filter` allows.
    [[nodiscard]] Result<std::vector<model::Tool>> tools(std::string_view server, const ToolFilter& filter) const;
    // Calls a tool; refused without contacting the server when `filter` doesn't allow it.
    [[nodiscard]] Result<model::CallToolResult> call_tool(std::string_view server, std::string_view tool,
                                                          std::optional<model::JsonObject> arguments, const ToolFilter& filter) const;
    // The resources a running server declares; empty without the resources capability.
    [[nodiscard]] Result<std::vector<model::Resource>> list_resources(std::string_view server) const;
    [[nodiscard]] Result<std::vector<model::ResourceContents>> read_resource(std::string_view server, std::string_view uri) const;
    // The `surface://` resources a running server declares.
    [[nodiscard]] Result<std::vector<std::pair<contracts::surface::SurfaceUri, model::Resource>>> surfaces(std::string_view server) const;
    // Connects to `spec` once and lists its tools: the check run before a server is saved.
    [[nodiscard]] Result<std::vector<model::Tool>> probe(const ServerSpec& spec) const;

    struct Inner;

  private:
    explicit Manager(std::shared_ptr<Inner> inner) : inner_(std::move(inner)) {}
    std::shared_ptr<Inner> inner_;
};

} // namespace loa::mcp
