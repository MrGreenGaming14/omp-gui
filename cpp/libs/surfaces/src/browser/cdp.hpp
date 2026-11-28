#pragma once

// A minimal Chrome DevTools Protocol client over an RFC 6455 WebSocket on Asio
// (local `ws://` only), replacing chromiumoxide. Private to the browser module.
//
// One `Connection` talks to the browser endpoint; pages are flat sessions on
// it (`Target.attachToTarget` with `flatten`), addressed by `sessionId`.

#include <asio/any_io_executor.hpp>
#include <asio/awaitable.hpp>

#include <chrono>
#include <cstdint>
#include <functional>
#include <memory>
#include <span>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/runtime/json.hpp"
#include "loa/surfaces/browser/error.hpp"

namespace loa::surfaces::browser::cdp {

using loa::runtime::json::Value;

// How long a command may take before it fails (chromiumoxide's request timeout).
inline constexpr std::chrono::seconds REQUEST_TIMEOUT{30};

// Called on the connection's reader with an event's `params`.
using EventHandler = std::function<void(const Value& params)>;

class Connection : public std::enable_shared_from_this<Connection> {
  public:
    // Opens `ws://host:port/path` and starts reading.
    [[nodiscard]] static asio::awaitable<Result<std::shared_ptr<Connection>>> connect(asio::any_io_executor executor,
                                                                                      std::string url);
    ~Connection();

    // Sends `method` with `params` on `session` (empty: the browser) and
    // waits for its result.
    [[nodiscard]] asio::awaitable<Result<Value>> call(std::string method, Value params, std::string session = {},
                                                      std::chrono::milliseconds timeout = REQUEST_TIMEOUT);
    // Sends without waiting for the answer.
    void send(std::string method, Value params, std::string session = {});

    // Calls `handler` for every `method` event on `session`. Returns a token for `unsubscribe`.
    std::uint64_t subscribe(std::string session, std::string method, EventHandler handler);
    void unsubscribe(std::uint64_t token);

    // Whether the socket is gone.
    [[nodiscard]] bool closed() const;
    // Closes the socket; pending calls fail.
    void close();

    struct Impl;

  private:
    explicit Connection(std::shared_ptr<Impl> impl) : impl_(std::move(impl)) {}
    std::shared_ptr<Impl> impl_;
};

// One attached page target.
struct Page {
    std::shared_ptr<Connection> conn;
    std::string target_id;
    std::string session_id;

    [[nodiscard]] asio::awaitable<Result<Value>> call(std::string method, Value params = loa::runtime::json::Object{}) const {
        return conn->call(std::move(method), std::move(params), session_id);
    }
};

// RFC 4648 base64 (standard alphabet, padding optional). Empty on bad input.
[[nodiscard]] std::optional<std::vector<std::uint8_t>> base64_decode(std::string_view text);

// Width and height from a JPEG's frame header.
[[nodiscard]] std::optional<std::pair<std::uint32_t, std::uint32_t>> jpeg_size(std::span<const std::uint8_t> jpeg);

} // namespace loa::surfaces::browser::cdp
