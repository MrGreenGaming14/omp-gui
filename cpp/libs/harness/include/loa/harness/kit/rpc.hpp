#pragma once

// Line-delimited JSON RPC over a child's stdio. A `Framing` strategy maps the harness's
// envelope (omp frames, JSON-RPC 2.0, claude control) onto `Incoming`; `StdioRpc`
// correlates responses by id, writes through one writer coroutine, serves inbound requests
// through the `Inbound` port and reports the process exit.

#include <chrono>
#include <concepts>
#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/runtime/error.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/json.hpp"

namespace loa::harness::kit {

using Json = loa::runtime::json::Value;
using loa::contracts::Awaitable;
using loa::runtime::exec::ExitStatus;

class RpcError {
  public:
    // The executable couldn't be started because it doesn't exist.
    struct NotFound { std::string program; };
    struct Io { loa::Error error; };
    // The process exited, or closed its stdout, while the client still needed it.
    // `stderr` holds the tail of what it wrote to stderr.
    struct Exited { std::optional<ExitStatus> status; std::string stderr; };
    // The harness speaks no protocol version this client supports.
    struct Version { std::vector<std::uint32_t> offered; std::uint32_t min = 0; std::uint32_t max = 0; };
    struct Protocol { std::string detail; };
    // A request failed on the harness side.
    struct Command { std::string command; std::string message; std::optional<std::string> code; };
    struct StartTimeout { std::chrono::nanoseconds after; };
    struct Closed {};
    // Malformed JSON; `detail` is the parser's message.
    struct Json { std::string detail; };

    using Kind = std::variant<NotFound, Io, Exited, Version, Protocol, Command, StartTimeout, Closed, Json>;
    // `Closed`; Asio needs coroutine results to be default constructible.
    RpcError() : kind_(Closed{}) {}

    template <typename K>
        requires std::is_constructible_v<Kind, K&&>
    RpcError(K&& kind) : kind_(std::forward<K>(kind)) {}

    [[nodiscard]] const Kind& kind() const noexcept { return kind_; }
    template <typename K>
    [[nodiscard]] const K* get() const noexcept { return std::get_if<K>(&kind_); }
    // The machine readable code of a failed request (`session_busy`, …).
    [[nodiscard]] std::optional<std::string_view> code() const noexcept;
    // The Rust `Display` text.
    [[nodiscard]] std::string message() const;
    // `NotFound` -> `NotInstalled`, `Io` -> `Io`, `Closed` -> `Closed`, the rest -> `Protocol(message)`.
    [[nodiscard]] loa::contracts::AdapterError to_adapter_error() const;

  private:
    Kind kind_;
};

// Rust's `Duration` `Debug` text (`20s`, `1.5s`, `250ms`).
[[nodiscard]] std::string duration_debug(std::chrono::nanoseconds d);

template <typename T = Json>
using RpcResult = std::expected<T, RpcError>;

// One decoded line, as the framing classifies it.
namespace incoming {
// The answer to a request this side sent; `id` as `id_key` renders it.
struct Response { std::string id; RpcResult<Json> result; };
// A request the harness sends; answered with `Framing::response`.
struct Request { Json id; std::string method; Json params; };
struct Notification { std::string method; Json params; };
// The harness withdrew its request `id`.
struct Cancel { Json id; };
} // namespace incoming
using Incoming = std::variant<incoming::Response, incoming::Request, incoming::Notification, incoming::Cancel>;

// The envelope of one harness protocol.
class Framing {
  public:
    virtual ~Framing() = default;
    [[nodiscard]] virtual Json request(const Json& id, std::string_view method, Json params) const = 0;
    [[nodiscard]] virtual Json notification(std::string_view method, Json params) const = 0;
    // The reply to an inbound `incoming::Request` for `method`.
    [[nodiscard]] virtual Json response(Json id, std::string_view method, RpcResult<Json> result) const = 0;
    // `nullopt` when the line was consumed (a partial chunk) or is not a frame.
    [[nodiscard]] virtual std::optional<Incoming> classify(Json msg) const = 0;
};

// What the harness sends that isn't a response. Coroutines may throw only on cancellation.
class Inbound {
  public:
    virtual ~Inbound() = default;
    [[nodiscard]] virtual Awaitable<RpcResult<Json>> on_request(std::string method, Json params) const = 0;
    // Called in order; the read loop waits, so a slow consumer backpressures the harness.
    [[nodiscard]] virtual Awaitable<void> on_notification(std::string method, Json params) const = 0;
    [[nodiscard]] virtual Awaitable<void> on_closed(std::optional<ExitStatus> status, std::string stderr) const = 0;
    // True when requests go to `on_deferred` in read order, to be answered later with
    // `StdioRpc::respond`, instead of being served on their own task.
    [[nodiscard]] virtual bool defers_requests() const { return false; }
    [[nodiscard]] virtual Awaitable<void> on_deferred(Json id, std::string method, Json params) const;
    // Sees every response, in read order, before the request waiting for it does.
    [[nodiscard]] virtual Awaitable<void> on_response(std::string id, const RpcResult<Json>& result) const;
};

// The id as a pending-map key: a string id as is, anything else as JSON text.
[[nodiscard]] std::string id_key(const Json& id);

// A result as a request decodes it: a missing or `null` result becomes `{}`.
[[nodiscard]] Json decode(Json data);

namespace detail {
struct RpcCore;
}

// Writes frames to a `StdioRpc` while it lives; see `StdioRpcBase::writer`.
class Writer {
  public:
    RpcResult<void> send(const Json& frame) const;

  private:
    friend class StdioRpcBase;
    explicit Writer(std::weak_ptr<detail::RpcCore> core) : core_(std::move(core)) {}
    std::weak_ptr<detail::RpcCore> core_;
};

// The framing-independent part of `StdioRpc`.
class StdioRpcBase {
  public:
    StdioRpcBase(StdioRpcBase&&) noexcept = default;
    StdioRpcBase& operator=(StdioRpcBase&& other) noexcept;
    StdioRpcBase(const StdioRpcBase&) = delete;
    StdioRpcBase& operator=(const StdioRpcBase&) = delete;
    // Closes the child's stdin.
    ~StdioRpcBase();

    // A fresh request id number; ids are never reused.
    [[nodiscard]] std::uint64_t next_id() const;
    // Sends a request under `id` and waits for its result.
    [[nodiscard]] Awaitable<RpcResult<Json>> request_as(Json id, std::string method, Json params) const;
    // Sends a request under a fresh numeric id; the result goes through `decode`.
    [[nodiscard]] Awaitable<RpcResult<Json>> request(std::string method, Json params) const;
    RpcResult<void> notify(std::string_view method, Json params) const;
    // Answers an inbound request the `Inbound` port deferred.
    RpcResult<void> respond(Json id, std::string_view method, RpcResult<Json> result) const;
    // Writes a frame as is.
    RpcResult<void> send(const Json& frame) const;
    // A handle that writes frames without keeping the process alive.
    [[nodiscard]] Writer writer() const;
    [[nodiscard]] bool has_exited() const;
    [[nodiscard]] int pid() const;
    // Resolves once the process is gone, with the error requests now fail with.
    [[nodiscard]] Awaitable<RpcError> closed() const;
    // Kills the process without waiting.
    void kill() const;
    // Closes stdin so the harness finishes queued work and exits, waits up to `grace`,
    // then kills it. Returns the exit status when known.
    [[nodiscard]] Awaitable<std::optional<ExitStatus>> shutdown(std::chrono::milliseconds grace) const;

  protected:
    explicit StdioRpcBase(std::shared_ptr<detail::RpcCore> core) : core_(std::move(core)) {}
    // Spawns `cmd` with piped stdio; the reader and writer run on `executor`.
    [[nodiscard]] static RpcResult<std::shared_ptr<detail::RpcCore>> spawn_core(
        const asio::any_io_executor& executor, const loa::runtime::exec::Command& cmd,
        std::shared_ptr<const Framing> framing, std::shared_ptr<const Inbound> inbound);

  private:
    void close_out() const noexcept;
    std::shared_ptr<detail::RpcCore> core_;
};

// A running harness process spoken to through framing `F`. Destroying it closes stdin.
template <std::derived_from<Framing> F>
class StdioRpc : public StdioRpcBase {
  public:
    // Starts `cmd` with piped stdio. Everything the harness sends that isn't a response
    // goes to `inbound`.
    [[nodiscard]] static RpcResult<StdioRpc> spawn(const asio::any_io_executor& executor,
                                                   const loa::runtime::exec::Command& cmd, F framing,
                                                   std::shared_ptr<const Inbound> inbound) {
        auto f = std::make_shared<const F>(std::move(framing));
        auto core = spawn_core(executor, cmd, f, std::move(inbound));
        if (!core) return std::unexpected(std::move(core).error());
        return StdioRpc(std::move(*core), std::move(f));
    }

    [[nodiscard]] const F& framing() const noexcept { return *framing_; }

  private:
    StdioRpc(std::shared_ptr<detail::RpcCore> core, std::shared_ptr<const F> framing)
        : StdioRpcBase(std::move(core)), framing_(std::move(framing)) {}
    std::shared_ptr<const F> framing_;
};

} // namespace loa::harness::kit
