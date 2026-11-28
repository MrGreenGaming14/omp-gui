#pragma once

// Internal to the adapters: runs blocking work on its own thread and resumes the awaiting
// coroutine on its executor with the result (Rust `spawn_blocking`). Includes Asio.

#include <asio.hpp>

#include <optional>
#include <thread>
#include <type_traits>
#include <utility>

#include "loa/contracts/async.hpp"

namespace loa::app::adapters::detail {

template <typename F>
auto offload(F work) -> contracts::Awaitable<std::invoke_result_t<F&>> {
    using T = std::invoke_result_t<F&>;
    // Asio's awaitable handler needs a default-constructible result.
    auto out = co_await asio::async_initiate<const asio::use_awaitable_t<>&, void(std::optional<T>)>(
        [](auto handler, F work) {
            auto guard = asio::make_work_guard(asio::get_associated_executor(handler));
            std::thread([handler = std::move(handler), work = std::move(work), guard = std::move(guard)]() mutable {
                std::optional<T> result(work());
                auto executor = asio::get_associated_executor(handler);
                asio::post(executor, [handler = std::move(handler), result = std::move(result)]() mutable {
                    std::move(handler)(std::move(result));
                });
            }).detach();
        },
        asio::use_awaitable, std::move(work));
    co_return std::move(*out);
}

} // namespace loa::app::adapters::detail
