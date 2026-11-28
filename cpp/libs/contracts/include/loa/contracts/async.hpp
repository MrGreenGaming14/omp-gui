#pragma once

// Forward declarations only: ports declare coroutines without pulling Asio into
// their headers. Implementations include <asio.hpp> themselves.
namespace asio {
class any_io_executor;
template <typename T, typename Executor> class awaitable;
} // namespace asio

namespace loa::contracts {

// What an async-trait method becomes: a coroutine on the `loa::runtime` executor.
template <typename T>
using Awaitable = asio::awaitable<T, asio::any_io_executor>;

} // namespace loa::contracts
