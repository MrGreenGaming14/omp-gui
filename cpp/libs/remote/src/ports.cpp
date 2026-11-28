#include "loa/remote/ports.hpp"

#include "loa/core/schema.hpp"

namespace loa::remote {

std::optional<Approval> Approval::from_answer(std::string_view answer, std::optional<std::string> reason) {
    if (answer == loa::core::answer::ALLOW_ONCE) return allow_once();
    if (answer == loa::core::answer::ALLOW_SESSION) return allow_session();
    if (answer == loa::core::answer::DENY) return deny(std::move(reason));
    return std::nullopt;
}

std::optional<std::string_view> session_of(const Prompt& prompt) noexcept {
    if (const auto* r = std::get_if<ApprovalRequest>(&prompt)) return r->session_id;
    return std::nullopt;
}

namespace {

// Where a parked question's answer lands.
struct Slot {
    bool done = false;
    std::optional<bool> trust;
    std::optional<Approval> approval;
};

} // namespace

struct PromptQueueShared {
    mutable std::mutex mu;
    std::condition_variable cv;
    std::uint64_t next = 0;
    std::uint64_t generation = 0;
    std::map<std::uint64_t, std::pair<Prompt, std::shared_ptr<Slot>>> pending;
    std::uint64_t next_listener = 0;
    std::map<std::uint64_t, std::shared_ptr<std::function<void(std::uint64_t)>>> listeners;

    // Bumps the counter (lock held), then notifies waiters and listeners with the lock released.
    void bump(std::unique_lock<std::mutex>& lock) {
        const auto value = ++generation;
        std::vector<std::shared_ptr<std::function<void(std::uint64_t)>>> to_call;
        for (const auto& [_, l] : listeners) to_call.push_back(l);
        lock.unlock();
        cv.notify_all();
        for (const auto& l : to_call) (*l)(value);
    }

    // Parks `prompt` until answered or `cancel` fires; the slot when answered.
    std::shared_ptr<Slot> ask(Prompt prompt, const CancelToken& cancel) {
        auto slot = std::make_shared<Slot>();
        std::unique_lock lock(mu);
        const auto id = ++next;
        pending.emplace(id, std::pair{std::move(prompt), slot});
        bump(lock);
        auto wake = cancel.on_cancel([this] {
            std::lock_guard l(mu);
            cv.notify_all();
        });
        lock.lock();
        cv.wait(lock, [&] { return slot->done || cancel.cancelled(); });
        if (slot->done) return slot;
        if (pending.erase(id) > 0) bump(lock);
        return nullptr;
    }
};

PromptQueue::Subscription::~Subscription() {
    if (auto shared = shared_.lock()) {
        std::lock_guard lock(shared->mu);
        shared->listeners.erase(id_);
    }
}

PromptQueue::PromptQueue() : shared_(std::make_shared<PromptQueueShared>()) {}
PromptQueue::~PromptQueue() = default;

std::uint64_t PromptQueue::generation() const {
    std::lock_guard lock(shared_->mu);
    return shared_->generation;
}

std::uint64_t PromptQueue::wait_change(std::uint64_t seen, std::chrono::milliseconds timeout) const {
    std::unique_lock lock(shared_->mu);
    shared_->cv.wait_for(lock, timeout, [&] { return shared_->generation != seen; });
    return shared_->generation;
}

PromptQueue::Subscription PromptQueue::subscribe(std::function<void(std::uint64_t)> listener) const {
    std::lock_guard lock(shared_->mu);
    const auto id = ++shared_->next_listener;
    shared_->listeners.emplace(id, std::make_shared<std::function<void(std::uint64_t)>>(std::move(listener)));
    return Subscription(shared_, id);
}

std::vector<PromptView> PromptQueue::pending() const {
    std::lock_guard lock(shared_->mu);
    std::vector<PromptView> out;
    for (const auto& [id, entry] : shared_->pending) out.push_back(PromptView{id, entry.first});
    return out;
}

bool PromptQueue::answer(std::uint64_t id, bool trust) {
    std::unique_lock lock(shared_->mu);
    const auto it = shared_->pending.find(id);
    if (it == shared_->pending.end() || !std::holds_alternative<HostKeyPrompt>(it->second.first)) return false;
    it->second.second->trust = trust;
    it->second.second->done = true;
    shared_->pending.erase(it);
    shared_->bump(lock);
    return true;
}

bool PromptQueue::answer(std::uint64_t id, Approval approval) {
    std::unique_lock lock(shared_->mu);
    const auto it = shared_->pending.find(id);
    if (it == shared_->pending.end() || !std::holds_alternative<ApprovalRequest>(it->second.first)) return false;
    it->second.second->approval = std::move(approval);
    it->second.second->done = true;
    shared_->pending.erase(it);
    shared_->bump(lock);
    return true;
}

void PromptQueue::deny_session(std::string_view session_id) {
    for (const auto& view : pending()) {
        if (session_of(view.prompt) == session_id) answer(view.id, Approval::deny("the session ended"));
    }
}

bool PromptQueue::decide(HostKeyPrompt prompt, const CancelToken& cancel) {
    const auto slot = shared_->ask(std::move(prompt), cancel);
    return slot && slot->trust.value_or(false);
}

Approval PromptQueue::approve(ApprovalRequest request, const CancelToken& cancel) {
    const auto slot = shared_->ask(std::move(request), cancel);
    if (!slot || !slot->approval) return Approval::deny("no answer");
    return *slot->approval;
}

} // namespace loa::remote
