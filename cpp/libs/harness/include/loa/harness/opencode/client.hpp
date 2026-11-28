#pragma once

#include <chrono>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>

#include "loa/contracts/harness.hpp"
#include "loa/harness/opencode/event.hpp"

namespace loa::harness::opencode {

struct ServerAuth {
    std::string username, password;
};

// Owns a single /event transfer. stop() may be called from a different thread
// than next(); the owner must join that thread before destroying this stream.
class EventStream {
  public:
    struct Impl;
    explicit EventStream(std::unique_ptr<Impl> impl);
    ~EventStream();
    EventStream(EventStream&&) noexcept;
    EventStream& operator=(EventStream&&) noexcept;
    EventStream(const EventStream&) = delete;
    EventStream& operator=(const EventStream&) = delete;

    [[nodiscard]] loa::contracts::Result<std::optional<Event>> next();
    void stop() noexcept;

  private:
    std::unique_ptr<Impl> impl_;
};

class Client {
  public:
    Client(std::string base, std::string directory, std::optional<ServerAuth> auth = std::nullopt);
    [[nodiscard]] const std::string& base() const noexcept { return base_; }
    [[nodiscard]] loa::contracts::Result<json::Value> request(
        std::string_view method, std::string_view path, std::optional<json::Value> body = std::nullopt) const;
    // Returns HTTP status even for non-2xx. Non-JSON bodies become null.
    [[nodiscard]] loa::contracts::Result<std::pair<long, json::Value>> probe(
        std::string_view path, std::chrono::milliseconds timeout) const;
    [[nodiscard]] loa::contracts::Result<EventStream> events() const;

  private:
    std::string base_, directory_;
    std::optional<ServerAuth> auth_;
};

} // namespace loa::harness::opencode
