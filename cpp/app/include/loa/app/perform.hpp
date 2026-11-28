#pragma once

// Builds `Task` jobs from coroutines and blocking calls (iced `Task::perform`, and `perform` of
// `loa-app/src/area.rs`). Includes Asio: use from `.cpp` files only.

#include <utility>

#include <asio.hpp>

#include "loa/app/task.hpp"
#include "loa/contracts/async.hpp"

namespace loa::app {

namespace detail {
template <typename Work, typename Map>
loa::runtime::Task run_perform(Work work, Map map, Task::Emit send) {
    auto value = co_await work();
    send(map(std::move(value)));
}

template <typename Work, typename Map>
loa::runtime::Task run_blocking(Work work, Map map, Task::Emit send) {
    send(map(work()));
    co_return;
}
} // namespace detail

// Runs the coroutine `work()` returns on the executor and hands its result to `map`, whose
// message goes back to the app.
template <typename Work, typename Map>
[[nodiscard]] Task perform(Work work, Map map) {
    return Task::job([work = std::move(work), map = std::move(map)](Task::Emit send, loa::runtime::Executor& executor) mutable {
        executor.spawn(detail::run_perform(std::move(work), std::move(map), std::move(send)));
    });
}

// Runs the blocking `work` on an executor thread (Rust `spawn_blocking`) and hands its result
// to `map`.
template <typename Work, typename Map>
[[nodiscard]] Task blocking(Work work, Map map) {
    return Task::job([work = std::move(work), map = std::move(map)](Task::Emit send, loa::runtime::Executor& executor) mutable {
        executor.spawn(detail::run_blocking(std::move(work), std::move(map), std::move(send)));
    });
}

} // namespace loa::app
