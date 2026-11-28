#pragma once

// Ports the app implements, host key decisions and approvals, and `PromptQueue`, which
// implements both by parking each question until the user answers it by id.
//
// The ports block the calling (worker) thread; `cancel` fires when the caller gives up, and the
// question must then go away.

#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/remote/audit.hpp"
#include "loa/remote/cancel.hpp"
#include "loa/remote/known_hosts.hpp"

namespace loa::remote {

// Why a host key needs a decision: a new key, or other keys are on file (possible man in the middle).
struct HostKeyChange {
    enum class Kind { new_key, changed };
    Kind kind = Kind::new_key;
    // The keys on file, for `changed`.
    std::vector<KnownKey> previous;
    friend bool operator==(const HostKeyChange&, const HostKeyChange&) = default;
};

// An unknown or changed host key, presented by `hostname:port` while connecting to `host`.
struct HostKeyPrompt {
    std::string host;
    std::string hostname;
    std::uint16_t port = 22;
    std::string key_type;
    std::string fingerprint;
    HostKeyChange change;
    friend bool operator==(const HostKeyPrompt&, const HostKeyPrompt&) = default;
};

// Decides whether to trust a host key; `true` saves it to `known_hosts` and connects.
class HostKeyDecider {
  public:
    virtual ~HostKeyDecider() = default;
    [[nodiscard]] virtual bool decide(HostKeyPrompt prompt, const CancelToken& cancel) = 0;
};

// A write or command waiting for the user's approval.
struct ApprovalRequest {
    std::string session_id;
    std::string host;
    Action action = Action::command;
    // Path or command.
    std::string target;
    // Byte count, edit preview or working directory.
    std::string detail;
    friend bool operator==(const ApprovalRequest&, const ApprovalRequest&) = default;
};

// The user's answer to an `ApprovalRequest`.
struct Approval {
    enum class Kind {
        allow_once,
        // Allow this action kind on this host for the rest of the session.
        allow_session,
        deny,
    };
    Kind kind = Kind::deny;
    // Why, for `deny`.
    std::optional<std::string> reason;

    [[nodiscard]] static Approval allow_once() { return {Kind::allow_once, std::nullopt}; }
    [[nodiscard]] static Approval allow_session() { return {Kind::allow_session, std::nullopt}; }
    [[nodiscard]] static Approval deny(std::optional<std::string> reason = std::nullopt) { return {Kind::deny, std::move(reason)}; }
    // Maps a decision answer (`loa::core::answer`) to an approval.
    [[nodiscard]] static std::optional<Approval> from_answer(std::string_view answer, std::optional<std::string> reason);
    friend bool operator==(const Approval&, const Approval&) = default;
};

// Asks the user to approve a write or command.
class Approver {
  public:
    virtual ~Approver() = default;
    [[nodiscard]] virtual Approval approve(ApprovalRequest request, const CancelToken& cancel) = 0;
};

// A question waiting in a `PromptQueue`: a host key answered with `bool`, an approval with `Approval`.
using Prompt = std::variant<HostKeyPrompt, ApprovalRequest>;
// Session the question belongs to; host keys belong to none.
[[nodiscard]] std::optional<std::string_view> session_of(const Prompt& prompt) noexcept;

// A parked question and the id to answer it by.
struct PromptView {
    std::uint64_t id = 0;
    Prompt prompt;
    friend bool operator==(const PromptView&, const PromptView&) = default;
};

struct PromptQueueShared;

// `HostKeyDecider` and `Approver` for the app: questions wait until `answer`; a question whose
// caller gives up (its cancel token fires) leaves by itself.
class PromptQueue final : public HostKeyDecider, public Approver {
  public:
    // Keeps a listener registered until destroyed.
    class Subscription {
      public:
        Subscription(Subscription&&) noexcept = default;
        Subscription& operator=(Subscription&&) noexcept = default;
        ~Subscription();

      private:
        friend class PromptQueue;
        Subscription(std::weak_ptr<PromptQueueShared> shared, std::uint64_t id) : shared_(std::move(shared)), id_(id) {}
        std::weak_ptr<PromptQueueShared> shared_;
        std::uint64_t id_ = 0;
    };

    PromptQueue();
    ~PromptQueue() override;

    // A counter that ticks whenever a question arrives or leaves.
    [[nodiscard]] std::uint64_t generation() const;
    // Blocks until the counter moves past `seen` or `timeout` passes; returns the counter.
    std::uint64_t wait_change(std::uint64_t seen, std::chrono::milliseconds timeout) const;
    // Calls `listener` (on the thread that changed the queue) with each new counter value.
    [[nodiscard]] Subscription subscribe(std::function<void(std::uint64_t)> listener) const;

    // The waiting questions, oldest first.
    [[nodiscard]] std::vector<PromptView> pending() const;
    // Answers question `id`; false when it isn't waiting or takes the other reply type.
    bool answer(std::uint64_t id, bool trust);
    bool answer(std::uint64_t id, Approval approval);
    // Denies every approval a session is still waiting on (it ended).
    void deny_session(std::string_view session_id);

    [[nodiscard]] bool decide(HostKeyPrompt prompt, const CancelToken& cancel) override;
    [[nodiscard]] Approval approve(ApprovalRequest request, const CancelToken& cancel) override;

  private:
    std::shared_ptr<PromptQueueShared> shared_;
};

} // namespace loa::remote
