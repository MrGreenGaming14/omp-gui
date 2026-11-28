#pragma once

// The single writer of a live session, the ids it is keyed by, and its
// `Persist`/`Authorize` ports.

#include <array>
#include <compare>
#include <cstddef>
#include <cstdint>
#include <expected>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/bus/queue.hpp"
#include "loa/core/sink.hpp"
#include "loa/runtime/error.hpp"

namespace loa::bus {

// Id of a session, the `id` attribute of its `<session>`. Cheap to copy:
// copies share one immutable string.
class SessionId {
  public:
    SessionId() : id_(empty()) {}
    explicit SessionId(std::string id) : id_(std::make_shared<const std::string>(std::move(id))) {}
    SessionId(std::string_view id) : SessionId(std::string(id)) {}
    SessionId(const char* id) : SessionId(std::string(id)) {}

    [[nodiscard]] std::string_view as_str() const noexcept { return *id_; }
    [[nodiscard]] const std::string& str() const noexcept { return *id_; }

    friend bool operator==(const SessionId& a, const SessionId& b) noexcept { return a.as_str() == b.as_str(); }
    friend auto operator<=>(const SessionId& a, const SessionId& b) noexcept { return a.as_str() <=> b.as_str(); }
    friend bool operator==(const SessionId& a, std::string_view b) noexcept { return a.as_str() == b; }
    friend auto operator<=>(const SessionId& a, std::string_view b) noexcept { return a.as_str() <=> b; }

  private:
    static const std::shared_ptr<const std::string>& empty();
    std::shared_ptr<const std::string> id_;
};

// A node's ed25519 public key: its network address and the identity the
// single writer rule and peer authorization are checked against. Only the
// public 32 bytes; never a signing secret.
struct NodeKey {
    std::array<std::uint8_t, 32> bytes{};
    using Signature = std::array<std::uint8_t, 64>;

    // The public key of the ed25519 private `seed` (OpenSSL).
    [[nodiscard]] static std::expected<NodeKey, loa::Error> from_seed(std::span<const std::uint8_t, 32> seed);
    [[nodiscard]] static std::expected<Signature, loa::Error> sign(
        std::span<const std::uint8_t, 32> seed, std::span<const std::uint8_t> message);
    [[nodiscard]] std::expected<bool, loa::Error> verify(
        std::span<const std::uint8_t> message, std::span<const std::uint8_t, 64> signature) const;

    // Rust `Debug`: `NodeKey(` the first 4 bytes as hex `…)`.
    [[nodiscard]] std::string debug() const;

    friend bool operator==(const NodeKey&, const NodeKey&) = default;
    friend auto operator<=>(const NodeKey&, const NodeKey&) = default;
};

// Errors of `Persist::append`, displayed by message only (Rust
// `Box<dyn Error>` display).
using PersistError = loa::Error;

// Durable storage for a session's patches, normally its journal. A port.
class Persist {
  public:
    virtual ~Persist() = default;

    // Stores `patch`. Called in sequence order, after the patch was checked
    // against the DOM and before it is applied or published. An error fails
    // the commit and leaves the session unchanged.
    [[nodiscard]] virtual std::expected<void, PersistError> append(const core::Patch& patch) = 0;

    // Called with the DOM right after the patch `append` stored was applied,
    // still in sequence order and under the writer's lock. Storage writes
    // snapshots here. The patch is already durable, so this can't fail the
    // commit.
    virtual void applied(const core::Dom& dom) { (void)dom; }
};

// Why a peer's request was refused.
struct Denied {
    std::string reason;
    friend bool operator==(const Denied&, const Denied&) = default;
};

// Decides whether a peer may make a change to a session this node owns. A
// port: the app backs it with trust grants. Called from any thread.
class Authorize {
  public:
    virtual ~Authorize() = default;
    // Called at the point the request is ordered, with the DOM it would apply
    // to, so subtree scoped grants see the current tree.
    [[nodiscard]] virtual std::expected<void, Denied> authorize(const SessionId& session, const NodeKey& requester,
                                                                const core::Draft& draft,
                                                                const core::Dom& dom) const = 0;
};

namespace request_error {
struct Conflict {
    std::uint64_t expected = 0;
    std::uint64_t actual = 0;
    friend bool operator==(const Conflict&, const Conflict&) = default;
};
} // namespace request_error

using RequestError = std::variant<Denied, request_error::Conflict, core::CommitError>;

namespace replicate_error {
// This node orders the session itself and takes no patches from others.
struct Owner {
    friend bool operator==(const Owner&, const Owner&) = default;
};
} // namespace replicate_error

using ReplicateError = std::variant<replicate_error::Owner, core::CommitError>;

// The Rust `Display` texts.
[[nodiscard]] std::string to_string(const Denied& e);
[[nodiscard]] std::string to_string(const RequestError& e);
[[nodiscard]] std::string to_string(const ReplicateError& e);
[[nodiscard]] loa::Error to_error(const RequestError& e);
[[nodiscard]] loa::Error to_error(const ReplicateError& e);

// Everything a `SessionWriter` starts from.
struct WriterConfig {
    SessionId session;
    // Key of the node that orders this session's patches.
    NodeKey owner;
    // Key of this node.
    NodeKey local;
    // The session as persisted so far (a new DOM, or the journal replayed).
    core::Dom dom;
    std::unique_ptr<Persist> persist;
    std::shared_ptr<const Authorize> authorize;
};

// The `PatchSink` of one live session.
//
// On the owner node it orders local drafts (`commit_at`) and peer requests
// (`request`) into one gap free sequence, in the order they reach it. On any
// other node it only mirrors the owner's patches (`replicate`). Each patch is
// checked, persisted, applied and then published to subscribers, all under one
// lock, so every subscriber sees the same order. Publishing never waits on a
// subscriber.
class SessionWriter final : public core::PatchSink {
  public:
    explicit SessionWriter(WriterConfig config);
    ~SessionWriter() override;
    SessionWriter(const SessionWriter&) = delete;
    SessionWriter& operator=(const SessionWriter&) = delete;

    [[nodiscard]] const SessionId& session() const noexcept { return session_; }
    [[nodiscard]] NodeKey owner() const noexcept { return owner_; }
    // Whether this node orders the session's patches.
    [[nodiscard]] bool is_writer() const noexcept { return owner_ == local_; }

    // Sequence of the last committed patch.
    [[nodiscard]] std::uint64_t seq() const;

    // Reads the current DOM without copying it. Commits wait while `read` runs.
    template <class F>
    decltype(auto) with_dom(F&& read) const {
        std::scoped_lock lock(mutex_);
        return std::forward<F>(read)(static_cast<const core::Dom&>(dom_));
    }

    // The current DOM and a subscription that starts with the patch right
    // after it.
    [[nodiscard]] std::pair<core::Dom, Subscription> subscribe(Policy policy);
    // The current sequence and a subscription that starts with the patch right
    // after it. For subscribers that read the journal up to that sequence
    // instead of copying the DOM.
    [[nodiscard]] std::pair<std::uint64_t, Subscription> subscribe_seq(Policy policy);

    // Orders a change requested by peer `requester`, after `Authorize` allows
    // it. With `expected_seq`, the change is refused unless the owner still has
    // the revision the peer saw. Both checks run under the writer lock.
    [[nodiscard]] std::expected<core::Patch, RequestError> request(const NodeKey& requester,
                                                                   std::optional<std::uint64_t> expected_seq,
                                                                   core::Draft draft, std::int64_t ts);

    // Mirrors a patch the owner ordered. Only on nodes that don't own the
    // session; patches must arrive in sequence.
    [[nodiscard]] std::expected<void, ReplicateError> replicate(core::Patch patch);

    // Ends the session: later commits fail with `Closed` and subscriptions end
    // after what they already hold.
    void close();

    [[nodiscard]] std::expected<core::Patch, core::CommitError> commit_at(core::Draft draft,
                                                                          std::int64_t ts) override;

  private:
    [[nodiscard]] std::expected<void, core::CommitError> writable() const;
    // Checks, persists and applies the next patch. Caller holds the lock.
    [[nodiscard]] std::expected<void, core::CommitError> store(const core::Patch& patch);
    // Publishes to the subscribers; caller holds the lock. Returns the patch
    // back when `keep`, without copying unless a subscriber still holds it.
    std::optional<core::Patch> publish(core::Patch patch, bool keep);
    Subscription attach(Policy policy);
    void shut();

    SessionId session_;
    NodeKey owner_;
    NodeKey local_;
    std::shared_ptr<const Authorize> authorize_;

    mutable std::mutex mutex_;
    core::Dom dom_;
    std::unique_ptr<Persist> persist_;
    std::vector<std::shared_ptr<detail::Queue>> subs_;
    bool closed_ = false;
};

} // namespace loa::bus

template <>
struct std::hash<loa::bus::SessionId> {
    using is_transparent = void;
    std::size_t operator()(std::string_view id) const noexcept { return std::hash<std::string_view>{}(id); }
    std::size_t operator()(const loa::bus::SessionId& id) const noexcept { return (*this)(id.as_str()); }
};

template <>
struct std::hash<loa::bus::NodeKey> {
    std::size_t operator()(const loa::bus::NodeKey& key) const noexcept {
        return std::hash<std::string_view>{}(
            std::string_view(reinterpret_cast<const char*>(key.bytes.data()), key.bytes.size()));
    }
};
