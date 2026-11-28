#pragma once

// Internal: runs blocking work on its own thread and resumes the awaiting coroutine on its
// executor with the result (Rust `spawn_blocking`).

#include <asio.hpp>

#include <thread>
#include <type_traits>
#include <utility>

namespace loa::sandbox::detail {

template <typename F>
auto offload(F work) -> asio::awaitable<std::invoke_result_t<F&>> {
    using T = std::invoke_result_t<F&>;
    co_return co_await asio::async_initiate<const asio::use_awaitable_t<>&, void(T)>(
        [](auto handler, F work) {
            auto guard = asio::make_work_guard(asio::get_associated_executor(handler));
            std::thread([handler = std::move(handler), work = std::move(work), guard = std::move(guard)]() mutable {
                T result = work();
                auto executor = asio::get_associated_executor(handler);
                asio::post(executor, [handler = std::move(handler), result = std::move(result)]() mutable {
                    std::move(handler)(std::move(result));
                });
            }).detach();
        },
        asio::use_awaitable, std::move(work));
}

} // namespace loa::sandbox::detail
