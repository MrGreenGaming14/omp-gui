#pragma once

// `DrivenSession` coroutine bodies. Includes Asio: only `.cpp` files that instantiate a
// `DrivenSession` include this header.

#include <asio/awaitable.hpp>
#include <asio/any_io_executor.hpp>

#include "loa/harness/kit/mapper.hpp"

namespace loa::harness::kit {

template <typename M, typename O>
    requires DrivenPair<M, O>
Awaitable<loa::contracts::Result<>> DrivenSession<M, O>::feed(typename M::Event ev) const {
    loa::contracts::Result<std::vector<typename M::Reply>> replies;
    {
        std::scoped_lock lock(mutex_);
        loa::core::DomWriter w(*sink_);
        replies = mapper_.apply(std::move(ev), w);
        state_.set(mapper_.ledger().state());
    }
    if (!replies) co_return std::unexpected(std::move(replies).error());
    for (auto& reply : *replies) {
        auto sent = co_await ops_.reply(std::move(reply));
        if (!sent) co_return sent;
    }
    co_return loa::contracts::Result<>{};
}

template <typename M, typename O>
    requires DrivenPair<M, O>
void DrivenSession<M, O>::closed() const {
    std::scoped_lock lock(mutex_);
    mapper_.ledger_mut().close();
    state_.set(loa::contracts::SessionState::Closed);
}

template <typename M, typename O>
    requires DrivenPair<M, O>
Awaitable<loa::contracts::Result<>> DrivenSession<M, O>::send(loa::contracts::Prompt prompt) const {
    using loa::contracts::AdapterError;
    using loa::contracts::SessionState;
    {
        std::scoped_lock lock(mutex_);
        switch (mapper_.ledger().state()) {
        case SessionState::Closed: co_return std::unexpected(AdapterError(AdapterError::Closed{}));
        case SessionState::Idle: break;
        default: co_return std::unexpected(AdapterError(AdapterError::Busy{}));
        }
        loa::core::DomWriter w(*sink_);
        if (auto begun = mapper_.ledger_mut().begin(w, prompt); !begun)
            co_return std::unexpected(AdapterError(AdapterError::Commit{std::move(begun).error()}));
        state_.set(mapper_.ledger().state());
    }
    co_return co_await ops_.send(std::move(prompt));
}

template <typename M, typename O>
    requires DrivenPair<M, O>
Awaitable<loa::contracts::Result<>> DrivenSession<M, O>::interrupt() const {
    if (state() == loa::contracts::SessionState::Idle) co_return loa::contracts::Result<>{};
    co_return co_await ops_.interrupt();
}

template <typename M, typename O>
    requires DrivenPair<M, O>
Awaitable<loa::contracts::Result<>> DrivenSession<M, O>::answer(loa::core::NodeId decision,
                                                                loa::contracts::Answer answer) const {
    loa::contracts::Result<std::optional<typename M::Reply>> reply;
    {
        std::scoped_lock lock(mutex_);
        loa::core::DomWriter w(*sink_);
        reply = mapper_.answer(decision, answer, w);
        mapper_.ledger_mut().answered(decision);
        state_.set(mapper_.ledger().state());
    }
    if (!reply) co_return std::unexpected(std::move(reply).error());
    if (!*reply) co_return loa::contracts::Result<>{};
    co_return co_await ops_.reply(std::move(**reply));
}

template <typename M, typename O>
    requires DrivenPair<M, O>
Awaitable<loa::contracts::Result<>> DrivenSession<M, O>::set_model(loa::contracts::ModelSelection selection) const {
    co_return co_await ops_.set_model(std::move(selection));
}

template <typename M, typename O>
    requires DrivenPair<M, O>
Awaitable<loa::contracts::Result<>> DrivenSession<M, O>::wait_idle() const {
    if (co_await state_.wait_settled() == loa::contracts::SessionState::Idle) co_return loa::contracts::Result<>{};
    co_return std::unexpected(loa::contracts::AdapterError(loa::contracts::AdapterError::Closed{}));
}

template <typename M, typename O>
    requires DrivenPair<M, O>
Awaitable<loa::contracts::Result<>> DrivenSession<M, O>::shutdown() const {
    auto result = co_await ops_.shutdown();
    closed();
    co_return result;
}

} // namespace loa::harness::kit
