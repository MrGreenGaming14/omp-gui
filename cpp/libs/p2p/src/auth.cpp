#include "loa/p2p/auth.hpp"

#include "loa/p2p/wire.hpp"

#include <algorithm>

#include "loa/runtime/log.hpp"
#include "loa/runtime/time.hpp"

namespace loa::p2p {

using runtime::time::now_ms;
using trust::Action;
using trust::Request;

NodeError grant_error(const trust::TrustError& error) { return NodeError(NodeError::Kind::grant, error.to_string()); }

bool GrantGate::allows(const bus::NodeKey& peer, const Request& request, std::int64_t now) const {
    auto grants = live_grants(peer, now);
    return std::ranges::any_of(grants, [&](const trust::VerifiedGrant& g) { return g.authorize(request, now).has_value(); });
}

std::vector<trust::VerifiedGrant> GrantAccess::live_grants(const bus::NodeKey& peer, std::int64_t now) const {
    std::vector<trust::VerifiedGrant> held;
    {
        std::scoped_lock lock(mutex_);
        if (auto it = held_.find(peer); it != held_.end()) held = it->second;
    }
    std::erase_if(held, [&](const trust::VerifiedGrant& g) {
        return g.expires_ms() <= now ||
               std::ranges::any_of(g.chain(), [&](const trust::Link& link) { return store_->is_revoked(link.grant); });
    });
    return held;
}

void GrantAccess::set(const bus::NodeKey& peer, std::vector<trust::VerifiedGrant> grants) {
    std::scoped_lock lock(mutex_);
    held_.insert_or_assign(peer, std::move(grants));
}

std::vector<Action> GrantAccess::session_actions(const bus::NodeKey& peer, const trust::SessionFacts& facts) const {
    const auto now = now_ms();
    std::vector<Action> out;
    for (Action action : SESSION_ACTIONS)
        if (GrantGate::allows(peer, Request::session(action, facts), now)) out.push_back(action);
    return out;
}

bool GrantAccess::turn_visible(const bus::NodeKey& peer, const trust::SessionFacts& facts, core::NodeId turn) const {
    return GrantGate::allows(peer, Request::turn(Action::view, facts, turn), now_ms());
}

bool GrantAccess::allows_in(const bus::NodeKey& peer, const core::Dom& dom, Action action,
                            std::optional<core::NodeId> node) const {
    auto facts = trust::SessionFacts::from_dom(dom);
    if (!facts) return false;
    const auto now = now_ms();
    const auto turn = node ? trust::turn_of(dom, *node) : std::nullopt;
    auto grants = live_grants(peer, now);
    return std::ranges::any_of(grants, [&](const trust::VerifiedGrant& grant) {
        return grant.authorize(Request::session(action, *facts), now).has_value() ||
               (turn && grant.authorize(Request::turn(action, *facts, *turn), now).has_value());
    });
}

core::NodeId target(const core::Draft& draft) {
    return std::visit(
        [](const auto& d) -> core::NodeId {
            if constexpr (requires { d.parent; }) return d.parent;
            else return d.node;
        },
        draft.v);
}

std::expected<void, bus::Denied> GrantAccess::authorize(const bus::SessionId& session, const bus::NodeKey& requester,
                                                        const core::Draft& draft, const core::Dom& dom) const {
    if (allows_in(requester, dom, Action::drive, target(draft))) return {};
    return std::unexpected(bus::Denied{"no drive grant on " + session.str() + " for this peer"});
}

std::expected<void, bus::Denied> Refuse::authorize(const bus::SessionId&, const bus::NodeKey&, const core::Draft&,
                                                   const core::Dom&) const {
    return std::unexpected(bus::Denied{"this node does not own the session"});
}

GrantService::GrantService(std::shared_ptr<trust::GrantStore> store_, trust::NodeIdentity identity_,
                           std::int64_t renew_margin_ms_)
    : store(std::move(store_)), access(std::make_shared<GrantAccess>(store)), identity(std::move(identity_)),
      renew_margin_ms(renew_margin_ms_) {}

Result<> GrantService::challenge(const NodeId& peer, Stream& stream) const {
    auto challenge = trust::Challenge::random();
    if (!challenge) return std::unexpected(grant_error(challenge.error()));
    wire::Frame frame{wire::Body(wire::Challenge{Bytes(challenge->bytes.begin(), challenge->bytes.end())})};
    auto reply = wire::call<wire::Frame>(stream, frame);
    if (!reply) return std::unexpected(reply.error());
    const auto* present = reply->body ? std::get_if<wire::Present>(&*reply->body) : nullptr;
    if (!present) return fail(NodeError::Kind::protocol, "expected the requester's grants");
    const trust::Verifier verifier(key(), *store);
    const auto now = now_ms();
    std::vector<trust::VerifiedGrant> proven;
    for (const auto& grant : present->grants) {
        trust::Proof proof;
        if (grant.proof.size() != proof.bytes.size()) continue;
        std::ranges::copy(grant.proof, proof.bytes.begin());
        auto verified = verifier.verify(trust::Presented{grant.token, &proof}, peer, *challenge, now);
        if (verified) proven.push_back(std::move(*verified));
        else loa::runtime::log::debug("p2p", "presented grant from " + peer.to_string() + " refused: " + verified.error().to_string());
    }
    access->set(trust::to_bus(peer), std::move(proven));
    return {};
}

Result<> GrantService::present(const NodeId& peer, Stream& stream) const {
    auto frame = wire::expect<wire::Frame>(*stream.recv);
    if (!frame) return std::unexpected(frame.error());
    const auto* nonce = frame->body ? std::get_if<wire::Challenge>(&*frame->body) : nullptr;
    if (!nonce) return fail(NodeError::Kind::protocol, "expected a challenge");
    trust::Challenge challenge;
    if (nonce->nonce.size() != challenge.bytes.size()) return fail(NodeError::Kind::protocol, "challenge is not 32 bytes");
    std::ranges::copy(nonce->nonce, challenge.bytes.begin());
    auto held = store->received_all();
    if (!held) return std::unexpected(grant_error(held.error()));
    const auto now = now_ms();
    wire::Present out;
    for (auto& g : *held) {
        if (g.root != peer || g.refused_ms || g.expires_ms <= now) continue;
        auto proof = trust::prove(identity, challenge, g.token);
        out.grants.push_back({std::move(g.token), Bytes(proof.bytes.begin(), proof.bytes.end())});
    }
    return wire::send(*stream.send, wire::Frame{wire::Body(std::move(out))});
}

bool GrantService::covers(const NodeId& peer, const trust::Resource& resource, Action action) const {
    namespace r = trust::resource;
    std::optional<Request> request;
    if (std::holds_alternative<r::Sandbox>(resource)) request = Request::sandbox(action);
    else if (auto* ssh = std::get_if<r::Ssh>(&resource)) request = Request::ssh(action, ssh->host);
    else if (auto* surface = std::get_if<r::Surface>(&resource)) request = Request::surface(action, surface->uri);
    else if (auto* folder = std::get_if<r::Folder>(&resource)) request = Request::folder(action, folder->path);
    if (!request) return false;
    return access->GrantGate::allows(trust::to_bus(peer), *request, now_ms());
}

std::vector<GrantInfo> GrantService::infos() const {
    std::vector<GrantInfo> out;
    if (auto issued = store->issued())
        for (auto& r : *issued)
            out.push_back(GrantInfo{.id = std::move(r.id),
                                    .peer = r.holder,
                                    .direction = GrantDirection::given,
                                    .scopes = r.scopes.value_or(std::vector<trust::Scope>{}),
                                    .until_ms = r.until_ms,
                                    .expires_ms = std::nullopt,
                                    .renews_at_ms = std::nullopt,
                                    .reshare = r.reshare,
                                    .ended = r.revoked_ms.has_value()});
    if (auto received = store->received_all())
        for (auto& r : *received) {
            auto scopes = r.scopes();
            out.push_back(GrantInfo{.id = r.id,
                                    .peer = r.issuer,
                                    .direction = GrantDirection::received,
                                    .scopes = scopes ? std::move(*scopes) : std::vector<trust::Scope>{},
                                    .until_ms = std::nullopt,
                                    .expires_ms = r.expires_ms,
                                    .renews_at_ms = r.refused_ms ? std::nullopt
                                                                 : std::optional(r.expires_ms - renew_margin_ms),
                                    .reshare = r.reshare,
                                    .ended = r.refused_ms.has_value()});
        }
    return out;
}

bool GrantService::holds_from(const NodeId& peer) const {
    auto held = store->received_all();
    return held && std::ranges::any_of(*held, [&](const trust::ReceivedGrant& g) { return g.root == peer && !g.refused_ms; });
}

Result<> GrantService::receive(const NodeId& sender, std::span<const std::uint8_t> token,
                               std::span<const std::uint8_t> root_bytes) const {
    trust::PublicKey root = sender;
    if (!root_bytes.empty()) {
        if (root_bytes.size() != root.bytes.size()) return fail(NodeError::Kind::protocol, "root key is not 32 bytes");
        std::ranges::copy(root_bytes, root.bytes.begin());
    }
    auto received = store->receive(key(), root, token);
    if (!received) return std::unexpected(grant_error(received.error()));
    if (received->issuer != sender) return fail(NodeError::Kind::refused, "only a grant's issuer may deliver it");
    return {};
}

Result<> GrantService::revoked_by(const NodeId& sender, const trust::GrantId& id) const {
    auto held = store->received(id);
    if (!held) return std::unexpected(grant_error(held.error()));
    if (!*held) return {};
    if ((*held)->issuer != sender) return fail(NodeError::Kind::refused, "only a grant's issuer may revoke it");
    if (auto r = store->record_revocation(id, (*held)->issuer, now_ms()); !r) return std::unexpected(grant_error(r.error()));
    if (auto r = store->mark_refused(id, now_ms()); !r) return std::unexpected(grant_error(r.error()));
    return {};
}

} // namespace loa::p2p
