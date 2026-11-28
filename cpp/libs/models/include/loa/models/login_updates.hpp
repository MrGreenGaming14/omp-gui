#pragma once

// The channel `Backend::login` reports progress on (tokio `UnboundedReceiver` in the
// Rust app). Pulls in Asio: only the app and tests include it.

#include <cstddef>

#include <asio/any_io_executor.hpp>
#include <asio/experimental/channel.hpp>

#include "loa/contracts/login_channels.hpp"
#include "loa/models/service.hpp"

namespace loa::models {

struct LoginUpdates {
    explicit LoginUpdates(const asio::any_io_executor& executor, std::size_t capacity = 256) : channel(executor, capacity) {}

    // Receives each update; a receive fails once the login has ended and every update
    // was taken.
    contracts::LoginUpdateChannel channel;
};

} // namespace loa::models
