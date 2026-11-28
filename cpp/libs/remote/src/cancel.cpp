#include "loa/remote/cancel.hpp"

#include <condition_variable>
#include <map>
#include <mutex>
#include <vector>

namespace loa::remote {

struct CancelState {
    std::mutex mu;
    std::condition_variable cv;
    bool cancelled = false;
    std::uint64_t next = 0;
    std::map<std::uint64_t, std::function<void()>> callbacks;
};

CancelToken::CancelToken() : state_(std::make_shared<CancelState>()) {}

void CancelToken::cancel() const {
    std::map<std::uint64_t, std::function<void()>> callbacks;
    {
        std::lock_guard lock(state_->mu);
        if (state_->cancelled) return;
        state_->cancelled = true;
        callbacks.swap(state_->callbacks);
    }
    state_->cv.notify_all();
    for (auto& [_, fn] : callbacks) fn();
}

bool CancelToken::cancelled() const {
    std::lock_guard lock(state_->mu);
    return state_->cancelled;
}

bool CancelToken::wait_for(std::chrono::milliseconds timeout) const {
    std::unique_lock lock(state_->mu);
    return state_->cv.wait_for(lock, timeout, [&] { return state_->cancelled; });
}

CancelToken::Callback CancelToken::on_cancel(std::function<void()> fn) const {
    {
        std::lock_guard lock(state_->mu);
        if (!state_->cancelled) {
            const auto id = ++state_->next;
            state_->callbacks.emplace(id, std::move(fn));
            return Callback(state_, id);
        }
    }
    fn();
    return {};
}

CancelToken::Callback::~Callback() {
    if (!state_) return;
    std::lock_guard lock(state_->mu);
    state_->callbacks.erase(id_);
}

} // namespace loa::remote
