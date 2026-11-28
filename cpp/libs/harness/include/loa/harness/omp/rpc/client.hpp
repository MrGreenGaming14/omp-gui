#pragma once

// The process side: `omp --mode rpc` on `kit::StdioRpc` with `OmpFrames`, the `ready`
// handshake and protocol negotiation, host callbacks, and everything else delivered in order
// as `Incoming`.

#include <atomic>
#include <chrono>
#include <cstdint>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/harness/kit/rpc.hpp"
#include "loa/harness/omp/rpc/command.hpp"
#include "loa/harness/omp/rpc/event.hpp"
#include "loa/harness/omp/rpc/frames.hpp"
#include "loa/harness/omp/rpc/inbound.hpp"

namespace loa::harness::omp::rpc {

using loa::contracts::Awaitable;
using loa::harness::kit::ExitStatus;

// Lowest protocol version this client speaks.
inline constexpr std::uint32_t MIN_PROTOCOL = 1;
// Highest protocol version this client speaks.
inline constexpr std::uint32_t MAX_PROTOCOL = 2;

// `--mode` of the child process.
enum class Mode {
    // `rpc`: extension UI, approvals and login prompts reach the host.
    Rpc,
    // `rpc-ui`: also enables the tool UI context, so the `ask` tool works.
    RpcUi,
};

// Serves calls omp makes back into the host.
class HostHandler {
  public:
    virtual ~HostHandler() = default;
    // Runs a host tool registered with `set_host_tools`. May throw only on cancellation.
    [[nodiscard]] virtual Awaitable<HostToolOutput> tool_call(HostToolCall call) = 0;
};

// How to start omp.
struct SpawnOptions {
    // The executable. A bare name is looked up on `PATH`.
    std::filesystem::path executable = "omp";
    Mode mode = Mode::Rpc;
    // Pass `--no-ui`: extensions run headless.
    bool no_ui = false;
    // Further CLI arguments, after `--mode`.
    std::vector<std::string> args;
    std::optional<std::filesystem::path> cwd;
    // Extra environment variables for the child.
    std::vector<std::pair<std::string, std::string>> env;
    // Variables of this process the child must not inherit.
    std::vector<std::string> env_remove;
    // How long to wait for the `ready` frame.
    std::chrono::milliseconds ready_timeout = std::chrono::seconds(60);
    // Negotiate protocol v2 when omp offers it.
    bool negotiate = true;
    // Frames buffered for the `Events` reader before omp is backpressured.
    std::size_t event_capacity = 1024;
    std::shared_ptr<HostHandler> host;

    SpawnOptions() = default;
    explicit SpawnOptions(std::filesystem::path executable) : executable(std::move(executable)) {}

    SpawnOptions& arg(std::string a) {
        args.push_back(std::move(a));
        return *this;
    }

    // The command line `spawn` runs.
    [[nodiscard]] loa::runtime::exec::Command command() const;
};

// Everything omp sends that isn't the response to a command of this client.
namespace incoming {
struct Event { rpc::Event event; friend bool operator==(const Event&, const Event&) = default; };
// A UI request. Dialogs need `RpcClient::respond_ui`.
struct Ui { UiRequest request; friend bool operator==(const Ui&, const Ui&) = default; };
// omp is gone. Always the last item.
struct Exited {
    std::optional<ExitStatus> status;
    std::string stderr;
    friend bool operator==(const Exited& a, const Exited& b) {
        const bool same_status = a.status.has_value() == b.status.has_value() && (!a.status || a.status->raw == b.status->raw);
        return same_status && a.stderr == b.stderr;
    }
};
} // namespace incoming

using Incoming = std::variant<incoming::Event, incoming::Ui, incoming::Exited>;

namespace detail {
struct EventQueue;
struct Inner;
} // namespace detail

// The stream of `Incoming` frames, in the order omp wrote them; it ends after
// `incoming::Exited`. Dropping it only means nobody listens: responses still flow.
class Events {
  public:
    Events() = default;
    Events(Events&&) noexcept = default;
    Events& operator=(Events&&) noexcept;
    Events(const Events&) = delete;
    Events& operator=(const Events&) = delete;
    ~Events();

    // The next frame; `nullopt` once the stream ended.
    [[nodiscard]] Awaitable<std::optional<Incoming>> recv();

  private:
    friend class RpcClient;
    explicit Events(std::shared_ptr<detail::EventQueue> queue) : queue_(std::move(queue)) {}
    std::shared_ptr<detail::EventQueue> queue_;
};

// Answers UI requests for an `RpcClient` without keeping omp running; see
// `RpcClient::ui_responder`.
class UiResponder {
  public:
    [[nodiscard]] RpcResult<void> respond(const UiResponse& response) const;

  private:
    friend class RpcClient;
    explicit UiResponder(loa::harness::kit::Writer writer) : writer_(std::move(writer)) {}
    loa::harness::kit::Writer writer_;
};

class RpcClient;

// An `RpcClient` that doesn't keep omp running; see `RpcClient::downgrade`.
class WeakRpcClient {
  public:
    WeakRpcClient() = default;
    // The client, while any `RpcClient` copy is alive.
    [[nodiscard]] std::optional<RpcClient> upgrade() const;

  private:
    friend class RpcClient;
    explicit WeakRpcClient(std::weak_ptr<detail::Inner> inner) : inner_(std::move(inner)) {}
    std::weak_ptr<detail::Inner> inner_;
};

struct Spawned;

// A running `omp --mode rpc` process. Cheap to copy; every copy talks to the same process,
// which is killed when the last copy is destroyed.
class RpcClient {
  public:
    // An empty client; only a moved-from or default value, never returned by `spawn`.
    RpcClient() = default;

    // Starts omp, waits for its `ready` frame and negotiates the protocol version. Fails with
    // `RpcError::Version` when omp offers no version in `MIN_PROTOCOL..=MAX_PROTOCOL`.
    [[nodiscard]] static Awaitable<RpcResult<Spawned>> spawn(SpawnOptions opts);

    // The `ready` frame omp sent at startup.
    [[nodiscard]] const Ready& ready() const;
    // The protocol version in use after negotiation.
    [[nodiscard]] std::uint32_t protocol_version() const;
    [[nodiscard]] bool has_exited() const;

    // A fresh command id (`c<n>`); ids are never reused. Reserve one when frames that will
    // refer to the command (such as a prompt's `prompt_result`) must be expected before it is
    // sent, then send it with `request_as`.
    [[nodiscard]] std::string reserve_id() const;
    // Sends `command` under `id` and returns the response `data`; missing `data` is `{}`.
    [[nodiscard]] Awaitable<RpcResult<Json>> request_as(std::string id, Command command) const;
    // `request_as` under a fresh id.
    [[nodiscard]] Awaitable<RpcResult<Json>> request(Command command) const;
    // Typed overloads: definitions in `typed.hpp`, which includes Asio only for callers
    // that actually use coroutines to decode a response.
    template <typename T> [[nodiscard]] Awaitable<RpcResult<T>> request_as(std::string id, Command command) const;
    template <typename T> [[nodiscard]] Awaitable<RpcResult<T>> request(Command command) const;

    // Answers a dialog UI request.
    [[nodiscard]] RpcResult<void> respond_ui(const UiResponse& response) const;
    // Answers UI requests like `respond_ui` without keeping omp running: once every
    // `RpcClient` copy is gone, the writes fail.
    [[nodiscard]] UiResponder ui_responder() const;
    // A handle that sends commands while the session holds the client, but doesn't keep omp
    // running once every `RpcClient` copy is gone.
    [[nodiscard]] WeakRpcClient downgrade() const;

    // Kills omp without waiting.
    void kill() const;
    // Closes omp's stdin so it finishes queued work and exits, waits up to `grace`, then
    // kills it. Returns the exit status when known.
    [[nodiscard]] Awaitable<RpcResult<std::optional<ExitStatus>>> shutdown(std::chrono::milliseconds grace) const;

  private:
    friend class WeakRpcClient;
    explicit RpcClient(std::shared_ptr<detail::Inner> inner) : inner_(std::move(inner)) {}
    std::shared_ptr<detail::Inner> inner_;
};

// What `RpcClient::spawn` returns.
struct Spawned {
    RpcClient client;
    Events events;
};

// Runs `<executable> --version` and returns what it printed, trimmed.
[[nodiscard]] RpcResult<std::string> omp_version(const std::filesystem::path& executable);

} // namespace loa::harness::omp::rpc
