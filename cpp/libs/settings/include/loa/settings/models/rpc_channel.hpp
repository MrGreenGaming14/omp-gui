#pragma once

// The Asio channel `OmpRpc::login` forwards login UI frames on. Pulls in Asio: only
// RPC clients and the sign-in screen include it.

#include <cstddef>

#include <asio/any_io_executor.hpp>
#include <asio/experimental/channel.hpp>

#include "loa/settings/models/rpc.hpp"

namespace loa::settings::models::rpc {

struct LoginUiChannel {
    // `capacity` buffered frames; a login emits only a handful.
    explicit LoginUiChannel(const asio::any_io_executor& executor, std::size_t capacity = 64) : channel(executor, capacity) {}

    asio::experimental::channel<void(asio::error_code, LoginUiRequest)> channel;
};

} // namespace loa::settings::models::rpc
