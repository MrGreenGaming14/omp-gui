#include "loa/app_runtime/feed.hpp"

#include <asio/any_completion_handler.hpp>
#include <asio/async_result.hpp>
#include <asio/post.hpp>
#include <asio/this_coro.hpp>
#include <asio/use_awaitable.hpp>

namespace loa::app_runtime {

struct Signal::Impl {
    std::mutex mutex;
    bool pending = false;
    asio::any_completion_handler<void()> waiter;
    asio::any_io_executor executor;
};

Signal::Signal() : impl_(std::make_unique<Impl>()) {}
Signal::~Signal() = default;

void Signal::notify() {
    std::unique_lock lock(impl_->mutex);
    if (!impl_->waiter) {
        impl_->pending = true;
        return;
    }
    auto handler = std::move(impl_->waiter);
    auto ex = std::move(impl_->executor);
    lock.unlock();
    asio::post(ex, [handler = std::move(handler)]() mutable { std::move(handler)(); });
}

contracts::Awaitable<void> Signal::wait() {
    Impl& impl = *impl_;
    asio::any_io_executor ex = co_await asio::this_coro::executor;
    co_await asio::async_initiate<const asio::use_awaitable_t<>&, void()>(
        [&impl, ex](auto handler) {
            std::unique_lock lock(impl.mutex);
            if (impl.pending) {
                impl.pending = false;
                lock.unlock();
                asio::post(ex, [handler = std::move(handler)]() mutable { std::move(handler)(); });
                return;
            }
            impl.waiter = std::move(handler);
            impl.executor = ex;
        },
        asio::use_awaitable);
}

} // namespace loa::app_runtime
