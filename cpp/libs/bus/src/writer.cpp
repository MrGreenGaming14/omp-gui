#include "loa/bus/writer.hpp"

#include <asio.hpp>
#include <openssl/evp.h>

#include <algorithm>
#include <cstdio>
#include <deque>
#include <format>

namespace loa::bus {

// ---------------------------------------------------------------- ids

const std::shared_ptr<const std::string>& SessionId::empty() {
    static const auto id = std::make_shared<const std::string>();
    return id;
}

std::expected<NodeKey, loa::Error> NodeKey::from_seed(std::span<const std::uint8_t, 32> seed) {
    std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)> pkey(
        EVP_PKEY_new_raw_private_key(EVP_PKEY_ED25519, nullptr, seed.data(), seed.size()), &EVP_PKEY_free);
    if (!pkey) return std::unexpected(loa::Error(ErrorCode::invalid_argument, "invalid ed25519 seed"));
    NodeKey key;
    std::size_t len = key.bytes.size();
    if (EVP_PKEY_get_raw_public_key(pkey.get(), key.bytes.data(), &len) != 1 || len != key.bytes.size())
        return std::unexpected(loa::Error(ErrorCode::internal, "deriving the ed25519 public key failed"));
    return key;
}

std::expected<NodeKey::Signature, loa::Error> NodeKey::sign(
    std::span<const std::uint8_t, 32> seed, std::span<const std::uint8_t> message) {
    std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)> key(
        EVP_PKEY_new_raw_private_key(EVP_PKEY_ED25519, nullptr, seed.data(), seed.size()), &EVP_PKEY_free);
    std::unique_ptr<EVP_MD_CTX, decltype(&EVP_MD_CTX_free)> ctx(EVP_MD_CTX_new(), &EVP_MD_CTX_free);
    Signature signature;
    auto len = signature.size();
    if (!key || !ctx || EVP_DigestSignInit(ctx.get(), nullptr, nullptr, nullptr, key.get()) != 1 ||
        EVP_DigestSign(ctx.get(), signature.data(), &len, message.data(), message.size()) != 1 ||
        len != signature.size()) {
        return std::unexpected(loa::Error(ErrorCode::internal, "signing with ed25519 failed"));
    }
    return signature;
}

std::expected<bool, loa::Error> NodeKey::verify(
    std::span<const std::uint8_t> message, std::span<const std::uint8_t, 64> signature) const {
    std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)> key(
        EVP_PKEY_new_raw_public_key(EVP_PKEY_ED25519, nullptr, bytes.data(), bytes.size()), &EVP_PKEY_free);
    std::unique_ptr<EVP_MD_CTX, decltype(&EVP_MD_CTX_free)> ctx(EVP_MD_CTX_new(), &EVP_MD_CTX_free);
    if (key && ctx && EVP_DigestVerifyInit(ctx.get(), nullptr, nullptr, nullptr, key.get()) == 1) {
        const auto result = EVP_DigestVerify(ctx.get(), signature.data(), signature.size(),
                                           message.data(), message.size());
        if (result >= 0) return result == 1;
    }
    return std::unexpected(loa::Error(ErrorCode::internal, "verifying ed25519 failed"));
}

std::string NodeKey::debug() const {
    return std::format("NodeKey({:02x}{:02x}{:02x}{:02x}…)", bytes[0], bytes[1], bytes[2], bytes[3]);
}

// ---------------------------------------------------------------- queue

std::expected<void, core::ApplyError> Appends::apply_to(core::Dom& dom) && {
    if (auto r = dom.apply(core::Patch{first_seq, node, ts, core::op::Append{std::move(text)}}); !r) return r;
    // The remaining sequence numbers carry no text of their own: empty
    // appends advance the mirror's sequence without changing the node.
    for (std::uint64_t seq = first_seq + 1; seq <= last_seq; ++seq)
        if (auto r = dom.apply(core::Patch{seq, node, ts, core::op::Append{}}); !r) return r;
    return {};
}

std::expected<bool, core::ApplyError> apply_to(Event&& event, core::Dom& dom) {
    if (auto* p = std::get_if<event::Patch>(&event)) return dom.apply(*p->patch).transform([] { return true; });
    if (auto* a = std::get_if<Appends>(&event)) return std::move(*a).apply_to(dom).transform([] { return true; });
    return false;
}

std::string_view to_string(TryRecvError e) noexcept {
    return e == TryRecvError::Empty ? "no event is waiting" : "the subscription has ended";
}

namespace detail {

class Queue {
  public:
    explicit Queue(Policy p) : policy{std::max<std::size_t>(p.capacity, 1), p.coalesce_appends} {}

    // Hands the parked `recv` back to its executor. Unlocks `lock`.
    void wake(std::unique_lock<std::mutex>& lock) {
        auto handler = std::move(waiter);
        auto ex = std::move(waiter_executor);
        lock.unlock();
        if (handler) asio::post(ex, [handler = std::move(handler)]() mutable { std::move(handler)(); });
    }

    [[nodiscard]] bool ready() const { return !items.empty() || lagged || closed; }

    std::expected<Event, TryRecvError> take() {
        std::scoped_lock lock(mutex);
        if (!items.empty()) {
            Event item = std::move(items.front());
            items.pop_front();
            return item;
        }
        if (lagged) {
            auto resync_from = *lagged;
            lagged.reset();
            closed = true;
            return event::Lagged{resync_from};
        }
        return std::unexpected(closed ? TryRecvError::Closed : TryRecvError::Empty);
    }

    const Policy policy;
    std::mutex mutex;
    std::deque<Event> items;
    // Set when the queue overflowed: the sequence of the first patch not delivered.
    std::optional<std::uint64_t> lagged;
    bool closed = false;
    bool receiver_gone = false;
    // The `recv` parked on an empty queue.
    asio::any_completion_handler<void()> waiter;
    asio::any_io_executor waiter_executor;
};

namespace {
// Merges an append into the last queued event when both act on `node` and the
// sequence numbers are adjacent.
bool coalesce(Event& last, core::NodeId node, std::uint64_t seq, std::int64_t ts, std::string_view text) {
    if (auto* p = std::get_if<event::Patch>(&last)) {
        const core::Patch& prev = *p->patch;
        if (prev.node != node || prev.seq + 1 != seq) return false;
        const auto* prev_append = std::get_if<core::op::Append>(&prev.op);
        if (!prev_append) return false;
        std::string merged;
        merged.reserve(prev_append->text.size() + text.size());
        merged.append(prev_append->text).append(text);
        last = Appends{node, prev.seq, seq, ts, std::move(merged)};
        return true;
    }
    if (auto* a = std::get_if<Appends>(&last); a && a->node == node && a->last_seq + 1 == seq) {
        a->text.append(text);
        a->last_seq = seq;
        a->ts = ts;
        return true;
    }
    return false;
}
} // namespace

std::shared_ptr<Queue> make_queue(Policy policy) { return std::make_shared<Queue>(policy); }

bool push(Queue& q, const std::shared_ptr<const core::Patch>& patch) {
    std::unique_lock lock(q.mutex);
    if (q.receiver_gone || q.closed || q.lagged) return false;
    if (q.policy.coalesce_appends && !q.items.empty()) {
        if (const auto* append = std::get_if<core::op::Append>(&patch->op);
            append && coalesce(q.items.back(), patch->node, patch->seq, patch->ts, append->text)) {
            // The receiver was already woken for the event this merged into.
            return true;
        }
    }
    if (q.items.size() >= q.policy.capacity) {
        q.lagged = patch->seq;
        q.wake(lock);
        return false;
    }
    q.items.push_back(event::Patch{patch});
    q.wake(lock);
    return true;
}

void close(Queue& q) {
    std::unique_lock lock(q.mutex);
    q.closed = true;
    q.wake(lock);
}

Subscription subscription(std::shared_ptr<Queue> queue) { return Subscription(std::move(queue)); }

} // namespace detail

Subscription& Subscription::operator=(Subscription&& other) noexcept {
    if (this != &other) {
        detach();
        queue_ = std::move(other.queue_);
    }
    return *this;
}

Subscription::~Subscription() { detach(); }

void Subscription::detach() noexcept {
    if (!queue_) return;
    std::scoped_lock lock(queue_->mutex);
    queue_->receiver_gone = true;
    queue_->items.clear();
    queue_->waiter = {};
}

std::expected<Event, TryRecvError> Subscription::try_recv() { return queue_->take(); }

asio::awaitable<std::optional<Event>, asio::any_io_executor> Subscription::recv() {
    detail::Queue* q = queue_.get();
    const auto ex = co_await asio::this_coro::executor;
    for (;;) {
        if (auto item = q->take()) co_return std::move(*item);
        else if (item.error() == TryRecvError::Closed) co_return std::nullopt;
        // Parks until a push or close; the readiness check and the parking
        // share the queue lock, so no wake-up is missed.
        co_await asio::async_initiate<decltype(asio::use_awaitable), void()>(
            [q, &ex](auto handler) {
                std::unique_lock lock(q->mutex);
                if (!q->ready()) {
                    q->waiter = std::move(handler);
                    q->waiter_executor = ex;
                    return;
                }
                lock.unlock();
                asio::post(ex, std::move(handler));
            },
            asio::use_awaitable);
    }
}

std::size_t Subscription::pending() const {
    std::scoped_lock lock(queue_->mutex);
    return queue_->items.size();
}

// ---------------------------------------------------------------- errors

std::string to_string(const Denied& e) { return e.reason; }

std::string to_string(const RequestError& e) {
    return std::visit(
        [](const auto& v) -> std::string {
            using T = std::decay_t<decltype(v)>;
            if constexpr (std::is_same_v<T, Denied>) return "request denied: " + v.reason;
            else if constexpr (std::is_same_v<T, request_error::Conflict>)
                return std::format("stale session revision: expected {}, actual {}", v.expected, v.actual);
            else return core::to_string(v);
        },
        e);
}

std::string to_string(const ReplicateError& e) {
    if (const auto* commit = std::get_if<core::CommitError>(&e)) return core::to_string(*commit);
    return "this node owns the session";
}

loa::Error to_error(const RequestError& e) {
    if (const auto* commit = std::get_if<core::CommitError>(&e)) return core::to_error(*commit);
    auto code = std::holds_alternative<Denied>(e) ? ErrorCode::permission_denied : ErrorCode::already_exists;
    return loa::Error(code, to_string(e));
}

loa::Error to_error(const ReplicateError& e) {
    if (const auto* commit = std::get_if<core::CommitError>(&e)) return core::to_error(*commit);
    return loa::Error(ErrorCode::unsupported, to_string(e));
}

// ---------------------------------------------------------------- writer

SessionWriter::SessionWriter(WriterConfig config)
    : session_(std::move(config.session)), owner_(config.owner), local_(config.local),
      authorize_(std::move(config.authorize)), dom_(std::move(config.dom)), persist_(std::move(config.persist)) {}

SessionWriter::~SessionWriter() { shut(); }

std::uint64_t SessionWriter::seq() const {
    std::scoped_lock lock(mutex_);
    return dom_.seq();
}

Subscription SessionWriter::attach(Policy policy) {
    auto queue = detail::make_queue(policy);
    if (closed_) detail::close(*queue);
    else subs_.push_back(queue);
    return detail::subscription(std::move(queue));
}

std::pair<core::Dom, Subscription> SessionWriter::subscribe(Policy policy) {
    std::scoped_lock lock(mutex_);
    auto sub = attach(policy);
    return {dom_, std::move(sub)};
}

std::pair<std::uint64_t, Subscription> SessionWriter::subscribe_seq(Policy policy) {
    std::scoped_lock lock(mutex_);
    auto sub = attach(policy);
    return {dom_.seq(), std::move(sub)};
}

std::expected<core::Patch, RequestError> SessionWriter::request(const NodeKey& requester,
                                                                std::optional<std::uint64_t> expected_seq,
                                                                core::Draft draft, std::int64_t ts) {
    std::scoped_lock lock(mutex_);
    if (auto w = writable(); !w) return std::unexpected(RequestError(std::move(w.error())));
    auto actual = dom_.seq();
    if (expected_seq && *expected_seq != actual)
        return std::unexpected(RequestError(request_error::Conflict{*expected_seq, actual}));
    if (auto ok = authorize_->authorize(session_, requester, draft, dom_); !ok)
        return std::unexpected(RequestError(std::move(ok.error())));
    auto patch = dom_.prepare(std::move(draft), ts);
    if (auto s = store(patch); !s) return std::unexpected(RequestError(std::move(s.error())));
    return std::move(*publish(std::move(patch), true));
}

std::expected<void, ReplicateError> SessionWriter::replicate(core::Patch patch) {
    if (is_writer()) return std::unexpected(ReplicateError(replicate_error::Owner{}));
    std::scoped_lock lock(mutex_);
    if (closed_) return std::unexpected(ReplicateError(core::CommitError(core::commit_error::Closed{})));
    if (auto s = store(patch); !s) return std::unexpected(ReplicateError(std::move(s.error())));
    publish(std::move(patch), false);
    return {};
}

std::expected<core::Patch, core::CommitError> SessionWriter::commit_at(core::Draft draft, std::int64_t ts) {
    std::scoped_lock lock(mutex_);
    if (auto w = writable(); !w) return std::unexpected(std::move(w.error()));
    auto patch = dom_.prepare(std::move(draft), ts);
    if (auto s = store(patch); !s) return std::unexpected(std::move(s.error()));
    return std::move(*publish(std::move(patch), true));
}

void SessionWriter::close() {
    std::scoped_lock lock(mutex_);
    shut();
}

void SessionWriter::shut() {
    closed_ = true;
    for (auto& queue : subs_) detail::close(*queue);
    subs_.clear();
}

std::expected<void, core::CommitError> SessionWriter::writable() const {
    if (closed_) return std::unexpected(core::CommitError(core::commit_error::Closed{}));
    if (!is_writer()) return std::unexpected(core::CommitError(core::commit_error::NotWriter{}));
    return {};
}

std::expected<void, core::CommitError> SessionWriter::store(const core::Patch& patch) {
    if (auto c = dom_.check(patch); !c) return std::unexpected(core::CommitError(core::commit_error::Apply{c.error()}));
    if (auto p = persist_->append(patch); !p)
        return std::unexpected(core::CommitError(core::commit_error::Journal{p.error().message()}));
    // Already checked against this DOM, so this cannot fail.
    if (auto a = dom_.apply(patch); !a)
        return std::unexpected(core::CommitError(core::commit_error::Apply{a.error()}));
    persist_->applied(dom_);
    return {};
}

std::optional<core::Patch> SessionWriter::publish(core::Patch patch, bool keep) {
    if (subs_.empty()) {
        if (keep) return patch;
        return std::nullopt;
    }
    auto shared = std::make_shared<core::Patch>(std::move(patch));
    std::shared_ptr<const core::Patch> view = shared;
    std::erase_if(subs_, [&](const auto& queue) { return !detail::push(*queue, view); });
    view.reset();
    if (!keep) return std::nullopt;
    // No subscriber queued it (or all dropped it): take it back without a copy.
    if (shared.use_count() == 1) return std::move(*shared);
    return *shared;
}

} // namespace loa::bus
