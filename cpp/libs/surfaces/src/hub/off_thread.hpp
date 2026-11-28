#pragma once

// Runs blocking work (MCP calls, encoder probes) on its own thread and resumes the
// calling coroutine on its executor with the result. Private to the hub.

#include <asio.hpp>

#include <thread>
#include <utility>

namespace loa::surfaces::hub::detail {

template <typename T, typename Work>
asio::awaitable<T> off_thread(Work work) {
    co_return co_await asio::async_initiate<decltype(asio::use_awaitable), void(T)>(
        [work = std::move(work)](auto handler) mutable {
            std::thread([work = std::move(work), handler = std::move(handler)]() mutable {
                T result = work();
                auto executor = asio::get_associated_executor(handler);
                asio::post(executor, [handler = std::move(handler), result = std::move(result)]() mutable { handler(std::move(result)); });
            }).detach();
        },
        asio::use_awaitable);
}

} // namespace loa::surfaces::hub::detail
