#include "loa/harness/kit/mapper.hpp"

#include "sync.hpp"

namespace loa::harness::kit {

using loa::contracts::AdapterError;
using loa::contracts::SessionState;

Awaitable<loa::contracts::Result<>> HarnessOpsBase::set_model(loa::contracts::ModelSelection) const {
    co_return std::unexpected(AdapterError(AdapterError::Unsupported{harness(), "set_model"}));
}

struct StateWatch::Impl {
    mutable std::mutex mutex;
    SessionState state;
    mutable detail::WaitList waiters;
};

StateWatch::StateWatch(SessionState initial) : impl_(std::make_unique<Impl>()) { impl_->state = initial; }
StateWatch::~StateWatch() = default;

SessionState StateWatch::get() const {
    std::scoped_lock lock(impl_->mutex);
    return impl_->state;
}

void StateWatch::set(SessionState state) {
    {
        std::scoped_lock lock(impl_->mutex);
        impl_->state = state;
    }
    impl_->waiters.notify_all();
}

Awaitable<SessionState> StateWatch::wait_settled() const {
    auto executor = co_await asio::this_coro::executor;
    for (;;) {
        std::shared_ptr<detail::Signal> signal;
        {
            std::scoped_lock lock(impl_->mutex);
            if (impl_->state == SessionState::Idle || impl_->state == SessionState::Closed) co_return impl_->state;
            signal = impl_->waiters.prepare(executor);
        }
        co_await detail::WaitList::wait(signal);
    }
}

} // namespace loa::harness::kit
