#pragma once

// Include this only when using typed coroutine requests. Kept separate from the lightweight
// client declarations so Asio headers do not propagate through unrelated public headers.
#include <asio/awaitable.hpp>

#include "loa/harness/omp/rpc/client.hpp"

namespace loa::harness::omp::rpc {

template <typename T>
Awaitable<RpcResult<T>> RpcClient::request_as(std::string id, Command command) const {
    auto result = co_await request_as(std::move(id), std::move(command));
    if (!result) co_return std::unexpected(std::move(result).error());
    co_return decode_as<T>(*result);
}

template <typename T>
Awaitable<RpcResult<T>> RpcClient::request(Command command) const {
    co_return co_await request_as<T>(reserve_id(), std::move(command));
}

} // namespace loa::harness::omp::rpc
