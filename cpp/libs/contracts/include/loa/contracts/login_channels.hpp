#pragma once

// The Asio channels a `LoginStrategy` runs between (tokio mpsc in the Rust app).
// Only login implementations and the sign-in screen include this header; it
// pulls in Asio.

#include <cstddef>

#include <asio/any_io_executor.hpp>
#include <asio/experimental/channel.hpp>

#include "loa/contracts/account.hpp"

namespace loa::contracts {

using LoginControlChannel = asio::experimental::channel<void(asio::error_code, LoginControl)>;
using LoginUpdateChannel = asio::experimental::channel<void(asio::error_code, LoginUpdate)>;

struct LoginChannels {
    LoginChannels(const asio::any_io_executor& executor, std::size_t capacity)
        : control(executor, capacity), updates(executor, capacity) {}

    // What the sign-in screen sends; the login receives.
    LoginControlChannel control;
    // What the login reports; the sign-in screen receives.
    LoginUpdateChannel updates;
};

} // namespace loa::contracts
