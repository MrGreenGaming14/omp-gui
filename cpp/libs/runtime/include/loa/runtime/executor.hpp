#pragma once

#include <cstddef>
#include <memory>

// Forward declarations only: Asio stays out of public headers. Code that writes
// coroutines includes <asio.hpp> itself; `Task` is then `asio::awaitable<void>`.
namespace asio {
class io_context;
class any_io_executor;
template <typename T, typename Executor> class awaitable;
} // namespace asio

namespace loa::runtime {

using Task = asio::awaitable<void, asio::any_io_executor>;

// Owns the `asio::io_context` and its worker threads.
class Executor {
  public:
    // `workers` background `std::jthread`s run the context in addition to any
    // thread blocked in `run()`.
    explicit Executor(std::size_t workers = 0);
    ~Executor();
    Executor(const Executor&) = delete;
    Executor& operator=(const Executor&) = delete;

    // Runs the context on the calling thread until `stop()` is called.
    void run();
    // Makes every `run()` return and stops the workers. Thread-safe, idempotent.
    void stop();
    [[nodiscard]] bool stopped() const noexcept;

    // Schedules a coroutine; safe from any thread. Exceptions escaping the
    // coroutine are logged and swallowed.
    void spawn(Task task);

    [[nodiscard]] asio::io_context& context() noexcept;

  private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

} // namespace loa::runtime
