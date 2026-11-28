#pragma once

// Grants on this node (Rust `loa_p2p::auth`): the `GrantGate` every protocol
// checks, the shared challenge/present handshake, and `GrantService` over the
// grant store.

#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <set>
#include <span>
#include <vector>

#include "loa/bus/writer.hpp"
#include "loa/p2p/protocols/projection.hpp"
#include "loa/p2p/transport.hpp"
#include "loa/trust/key.hpp"
#include "loa/trust/store.hpp"
#include "loa/trust/token.hpp"

namespace loa::p2p {

// A grant store or token error as a node error (displayed transparently).
[[nodiscard]] NodeError grant_error(const trust::TrustError& error);

// Port: the grants a peer proved that still count, and what they allow.
class GrantGate {
  public:
    virtual ~GrantGate() = default;
    // Proven grants that are neither expired nor revoked at `now`.
    [[nodiscard]] virtual std::vector<trust::VerifiedGrant> live_grants(const bus::NodeKey& peer,
                                                                        std::int64_t now) const = 0;
    [[nodiscard]] bool allows(const bus::NodeKey& peer, const trust::Request& request, std::int64_t now) const;
};

// The session actions a grant can carry, strongest last.
inline constexpr trust::Action SESSION_ACTIONS[4] = {trust::Action::view, trust::Action::fork, trust::Action::suggest,
                                                     trust::Action::drive};

// The grants each peer last proved, checked again on every use. It backs the
// session `Access` and the `bus::Authorize` of every session writer.
class GrantAccess final : public GrantGate, public Access, public bus::Authorize {
  public:
    explicit GrantAccess(std::shared_ptr<const trust::GrantStore> store) : store_(std::move(store)) {}

    [[nodiscard]] std::vector<trust::VerifiedGrant> live_grants(const bus::NodeKey& peer,
                                                                std::int64_t now) const override;
    using GrantGate::allows;

    // Records the grants `peer` just proved, replacing earlier ones.
    void set(const bus::NodeKey& peer, std::vector<trust::VerifiedGrant> grants);

    [[nodiscard]] std::vector<trust::Action> session_actions(const bus::NodeKey& peer,
                                                             const trust::SessionFacts& facts) const;
    [[nodiscard]] bool turn_visible(const bus::NodeKey& peer, const trust::SessionFacts& facts, core::NodeId turn) const;
    // `action` on the session, or on the turn `node` is in.
    [[nodiscard]] bool allows_in(const bus::NodeKey& peer, const core::Dom& dom, trust::Action action,
                                 std::optional<core::NodeId> node) const;

    // `Access`.
    [[nodiscard]] bool allows(const bus::NodeKey& peer, const core::Dom& dom, trust::Action action,
                              std::optional<core::NodeId> node) const override {
        return allows_in(peer, dom, action, node);
    }
    // `bus::Authorize`: a drive grant on the draft's target.
    [[nodiscard]] std::expected<void, bus::Denied> authorize(const bus::SessionId& session,
                                                             const bus::NodeKey& requester, const core::Draft& draft,
                                                             const core::Dom& dom) const override;

  private:
    std::shared_ptr<const trust::GrantStore> store_;
    mutable std::mutex mutex_;
    std::map<bus::NodeKey, std::vector<trust::VerifiedGrant>> held_;
};

// The element a draft acts on.
[[nodiscard]] core::NodeId target(const core::Draft& draft);

// The `Authorize` of mirrors, which take no requests.
class Refuse final : public bus::Authorize {
  public:
    [[nodiscard]] std::expected<void, bus::Denied> authorize(const bus::SessionId&, const bus::NodeKey&,
                                                             const core::Draft&, const core::Dom&) const override;
};

// Whether a grant was given by this node or is held from a peer.
enum class GrantDirection { given, received };

// A grant this node issued or holds.
struct GrantInfo {
    trust::GrantId id;
    // The holder of a given grant, the issuer of a received one.
    NodeId peer;
    GrantDirection direction = GrantDirection::given;
    std::vector<trust::Scope> scopes;
    // When a given grant ends.
    std::optional<std::int64_t> until_ms;
    // When a received grant's token expires, and when it renews.
    std::optional<std::int64_t> expires_ms;
    std::optional<std::int64_t> renews_at_ms;
    bool reshare = false;
    // Revoked here, or refused renewal by its issuer.
    bool ended = false;

    // A received grant that has neither ended nor expired.
    [[nodiscard]] bool live_received(std::int64_t now) const {
        return direction == GrantDirection::received && !ended && (!expires_ms || *expires_ms > now);
    }

    friend bool operator==(const GrantInfo&, const GrantInfo&) = default;
};

// The grant store with this node's identity: issuing, receiving and proving.
class GrantService {
  public:
    GrantService(std::shared_ptr<trust::GrantStore> store, trust::NodeIdentity identity, std::int64_t renew_margin_ms);

    std::shared_ptr<trust::GrantStore> store;
    std::shared_ptr<GrantAccess> access;
    trust::NodeIdentity identity;
    std::int64_t renew_margin_ms;
    // Grants issued while their holder was unreachable.
    std::mutex undelivered_mutex;
    std::set<trust::GrantId> undelivered;

    [[nodiscard]] const trust::PublicKey& key() const noexcept { return identity.public_key(); }

    // Owner side of the handshake: challenges `peer` and records the grants it proves.
    Result<> challenge(const NodeId& peer, Stream& stream) const;
    // Requester side: answers `peer`'s challenge with every live grant held from it.
    Result<> present(const NodeId& peer, Stream& stream) const;

    // Whether `peer` proved a live grant for `action` on `resource` (sandbox, SSH, surface or folder).
    [[nodiscard]] bool covers(const NodeId& peer, const trust::Resource& resource, trust::Action action) const;
    // Grants issued here, then grants held.
    [[nodiscard]] std::vector<GrantInfo> infos() const;
    // Whether this node holds a grant from `peer` it still renews.
    [[nodiscard]] bool holds_from(const NodeId& peer) const;
    // Stores a grant `sender` delivered; only its issuer may deliver it.
    Result<> receive(const NodeId& sender, std::span<const std::uint8_t> token, std::span<const std::uint8_t> root) const;
    // Ends a held grant on its issuer's notice.
    Result<> revoked_by(const NodeId& sender, const trust::GrantId& id) const;
};

} // namespace loa::p2p
