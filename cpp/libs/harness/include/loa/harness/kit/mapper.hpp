#pragma once

// The generic session: an `EventMapper` turns decoded harness events into DOM writes and
// replies, `HarnessOps` talks to the process, and `DrivenSession` implements
// `HarnessSession` once over both.
//
// `DrivenSession`'s coroutine bodies live in `loa/harness/kit/mapper_impl.hpp`, which pulls
// in Asio: include it from the `.cpp` that instantiates a `DrivenSession`.

#include <concepts>
#include <memory>
#include <mutex>
#include <optional>
#include <string_view>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/contracts/harness.hpp"
#include "loa/core/sink.hpp"
#include "loa/core/writer.hpp"
#include "loa/harness/kit/ledger.hpp"

namespace loa::harness::kit {

using loa::contracts::Awaitable;

// Pure protocol-to-DOM translation of one harness. `Reply` is what the mapper needs sent
// back (auto-approvals, decision answers).
template <typename Ev, typename Rep>
class EventMapper {
  public:
    using Event = Ev;
    using Reply = Rep;

    virtual ~EventMapper() = default;
    virtual loa::contracts::Result<std::vector<Reply>> apply(Event ev, const loa::core::DomWriter& w) = 0;
    // The reply answering `decision`; `nullopt` when the harness needs none.
    virtual loa::contracts::Result<std::optional<Reply>> answer(loa::core::NodeId decision,
                                                                const loa::contracts::Answer& answer,
                                                                const loa::core::DomWriter& w) = 0;
    [[nodiscard]] virtual const TurnLedger<std::string>& ledger() const = 0;
    virtual TurnLedger<std::string>& ledger_mut() = 0;
};

// The process side of one session, without its reply type.
class HarnessOpsBase {
  public:
    virtual ~HarnessOpsBase() = default;
    [[nodiscard]] virtual std::string_view harness() const = 0;
    [[nodiscard]] virtual Awaitable<loa::contracts::Result<>> send(loa::contracts::Prompt prompt) const = 0;
    [[nodiscard]] virtual Awaitable<loa::contracts::Result<>> interrupt() const = 0;
    // `Unsupported` unless overridden.
    [[nodiscard]] virtual Awaitable<loa::contracts::Result<>> set_model(loa::contracts::ModelSelection selection) const;
    [[nodiscard]] virtual Awaitable<loa::contracts::Result<>> shutdown() const = 0;
};

// The process side of one session.
template <typename Rep>
class HarnessOps : public HarnessOpsBase {
  public:
    using Reply = Rep;
    [[nodiscard]] virtual Awaitable<loa::contracts::Result<>> reply(Reply reply) const = 0;
};

// The session state, observable from coroutines (the Rust `watch` channel).
class StateWatch {
  public:
    explicit StateWatch(loa::contracts::SessionState initial);
    ~StateWatch();
    StateWatch(const StateWatch&) = delete;
    StateWatch& operator=(const StateWatch&) = delete;

    [[nodiscard]] loa::contracts::SessionState get() const;
    void set(loa::contracts::SessionState state);
    // Resolves with the first state that is `Idle` or `Closed`.
    [[nodiscard]] Awaitable<loa::contracts::SessionState> wait_settled() const;

  private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

template <typename M, typename O>
concept DrivenPair = std::derived_from<M, EventMapper<typename M::Event, typename M::Reply>> &&
                     std::derived_from<O, HarnessOps<typename M::Reply>>;

// A `HarnessSession` driven by a mapper and the process operations.
template <typename M, typename O>
    requires DrivenPair<M, O>
class DrivenSession final : public loa::contracts::HarnessSession {
  public:
    DrivenSession(M mapper, O ops, std::shared_ptr<loa::core::PatchSink> sink)
        : mapper_(std::move(mapper)), ops_(std::move(ops)), sink_(std::move(sink)), state_(mapper_.ledger().state()) {}

    [[nodiscard]] const O& ops() const noexcept { return ops_; }

    // Applies one decoded event from the harness and sends the mapper's replies.
    [[nodiscard]] Awaitable<loa::contracts::Result<>> feed(typename M::Event ev) const;
    // The process is gone.
    void closed() const;

    [[nodiscard]] Awaitable<loa::contracts::Result<>> send(loa::contracts::Prompt prompt) const override;
    [[nodiscard]] Awaitable<loa::contracts::Result<>> interrupt() const override;
    [[nodiscard]] Awaitable<loa::contracts::Result<>> answer(loa::core::NodeId decision,
                                                             loa::contracts::Answer answer) const override;
    [[nodiscard]] Awaitable<loa::contracts::Result<>> set_model(loa::contracts::ModelSelection selection) const override;
    [[nodiscard]] std::string_view harness() const override { return ops_.harness(); }
    [[nodiscard]] loa::contracts::SessionState state() const override { return state_.get(); }
    [[nodiscard]] Awaitable<loa::contracts::Result<>> wait_idle() const override;
    [[nodiscard]] Awaitable<loa::contracts::Result<>> shutdown() const override;

  private:
    mutable std::mutex mutex_;
    mutable M mapper_;
    O ops_;
    std::shared_ptr<loa::core::PatchSink> sink_;
    mutable StateWatch state_;
};

} // namespace loa::harness::kit
