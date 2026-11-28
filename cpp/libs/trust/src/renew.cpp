#include "loa/trust/renew.hpp"

#include <asio.hpp>

#include "chain.hpp"

namespace loa::trust {

RenewFailure RenewFailure::from_issuer(const TrustError& error) {
    using K = TrustError::Kind;
    switch (error.kind()) {
        case K::revoked:
        case K::expired:
        case K::unknown_grant:
        case K::not_holder:
        case K::reshare_forbidden: return refused(error.to_string());
        default: return unreachable(error.to_string());
    }
}

std::string RenewFailure::to_string() const {
    return (kind == Kind::refused ? "the issuer refused to renew: " : "renewal failed: ") + reason;
}

std::optional<std::int64_t> Renewer::due_at(const ReceivedGrant& grant) const {
    if (grant.refused_ms) return std::nullopt;
    return detail::saturating_sub(grant.expires_ms, margin_ms_);
}

Result<std::optional<std::int64_t>> Renewer::next_due(const GrantStore& store) const {
    auto all = store.received_all();
    if (!all) return std::unexpected(all.error());
    std::optional<std::int64_t> next;
    for (const auto& grant : *all)
        if (const auto due = due_at(grant); due && (!next || *due < *next)) next = due;
    return next;
}

Awaitable<Result<std::vector<RenewOutcome>>> Renewer::renew_due(const GrantStore& store, const RenewTransport& transport,
                                                                std::int64_t now_ms) const {
    // Copied so the coroutine doesn't depend on `this` across suspensions.
    const auto own = own_;
    auto all = store.received_all();
    if (!all) co_return std::unexpected(all.error());
    std::vector<ReceivedGrant> due;
    for (auto& grant : *all)
        if (const auto at = due_at(grant); at && *at <= now_ms) due.push_back(std::move(grant));

    std::vector<RenewOutcome> outcomes;
    for (auto& grant : due) {
        const auto id = grant.id;
        auto token = co_await transport.renew(grant.issuer, grant.id);
        if (token) {
            auto fresh = store.receive_renewal(own, grant.root, id, *token);
            if (fresh) outcomes.emplace_back(renew_outcome::Renewed{id, fresh->expires_ms});
            else outcomes.emplace_back(renew_outcome::Failed{id, fresh.error().to_string()});
        } else if (token.error().kind == RenewFailure::Kind::refused) {
            if (auto marked = store.mark_refused(id, now_ms); !marked) co_return std::unexpected(marked.error());
            outcomes.emplace_back(renew_outcome::Refused{id, token.error().reason});
        } else {
            outcomes.emplace_back(renew_outcome::Failed{id, token.error().reason});
        }
    }
    co_return outcomes;
}

} // namespace loa::trust
