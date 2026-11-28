#pragma once

// Registry of live sessions.

#include <expected>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/bus/writer.hpp"

namespace loa::bus {

namespace bus_error {
struct AlreadyOpen {
    SessionId session;
    friend bool operator==(const AlreadyOpen&, const AlreadyOpen&) = default;
};
} // namespace bus_error

using BusError = std::variant<bus_error::AlreadyOpen>;

[[nodiscard]] std::string to_string(const BusError& e);
[[nodiscard]] loa::Error to_error(const BusError& e);

// The live sessions of this node. Thread-safe.
class Bus {
  public:
    Bus() = default;
    Bus(const Bus&) = delete;
    Bus& operator=(const Bus&) = delete;

    // Starts the writer of a session.
    [[nodiscard]] std::expected<std::shared_ptr<SessionWriter>, BusError> open(WriterConfig config);
    [[nodiscard]] std::shared_ptr<SessionWriter> get(std::string_view session) const;
    // Ids of the open sessions, in no particular order.
    [[nodiscard]] std::vector<SessionId> ids() const;
    // Closes a session: its writer refuses further commits and its
    // subscriptions end. Returns the writer, or null if it wasn't open.
    std::shared_ptr<SessionWriter> close(std::string_view session);

  private:
    mutable std::mutex mutex_;
    std::map<SessionId, std::shared_ptr<SessionWriter>, std::less<>> sessions_;
};

} // namespace loa::bus
