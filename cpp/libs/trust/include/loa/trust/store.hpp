#pragma once

// Grant definitions in SQLite: grants issued, grants received, revocations.
// Tokens live minutes; the issuer keeps each definition and mints a fresh
// token whenever the holder asks while it is active. Revoking stops renewal.

#include <cstdint>
#include <filesystem>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/trust/token.hpp"

struct sqlite3;

namespace loa::runtime::json {
class Value;
}

namespace loa::trust {

// What a new grant gives.
struct GrantSpec {
    std::vector<Scope> scopes;
    // Lifetime of each token; renewal hands out a new one.
    std::int64_t ttl_ms = 0;
    // Whether the holder may re-share a narrower grant.
    bool reshare = false;
    // When the grant ends: no token outlives it. None renews until revoked.
    std::optional<std::int64_t> until_ms;

    friend bool operator==(const GrantSpec&, const GrantSpec&) = default;
};

// How a holder narrows a grant it re-shares: rights are the intersection of
// the parent's and these, so a re-share never widens.
struct Narrowing {
    // Restrict to these scopes; none keeps the parent's.
    std::optional<std::vector<Scope>> scopes;
    // Lifetime of each token, capped by the parent token's expiry.
    std::int64_t ttl_ms = 0;
    bool reshare = false;

    friend bool operator==(const Narrowing&, const Narrowing&) = default;
};

// The stored definition of an issued grant: JSON tagged `"kind": "root"` or `"reshare"`.
using Definition = std::variant<GrantSpec, Narrowing>;

// The serde JSON forms stored in `grants_issued.spec`.
[[nodiscard]] runtime::json::Value scope_to_json(const Scope& scope);
[[nodiscard]] Result<Scope> scope_from_json(const runtime::json::Value& value);
[[nodiscard]] std::string definition_to_json(const Definition& definition);
[[nodiscard]] Result<Definition> definition_from_json(std::string_view text);

// A token minted for a holder.
struct IssuedGrant {
    GrantId id;
    PublicKey holder;
    std::vector<std::uint8_t> token;
    std::int64_t expires_ms = 0;

    friend bool operator==(const IssuedGrant&, const IssuedGrant&) = default;
};

// A grant this node holds.
struct ReceivedGrant {
    GrantId id;
    // Who renews it: the root, or the peer that re-shared it.
    PublicKey issuer;
    // The node whose resources it covers and that verifies it.
    PublicKey root;
    std::vector<std::uint8_t> token;
    std::int64_t expires_ms = 0;
    bool reshare = false;
    // When the issuer refused to renew it.
    std::optional<std::int64_t> refused_ms;

    // The rights its root signed into it; see `token_scopes`.
    [[nodiscard]] Result<std::vector<Scope>> scopes() const { return token_scopes(token, root); }

    friend bool operator==(const ReceivedGrant&, const ReceivedGrant&) = default;
};

// A grant this node issued, as stored.
struct IssuedRecord {
    GrantId id;
    PublicKey holder;
    // The rights given; none for a re-share that keeps its parent's.
    std::optional<std::vector<Scope>> scopes;
    std::int64_t ttl_ms = 0;
    bool reshare = false;
    // When a root grant ends; none until revoked (and for re-shares).
    std::optional<std::int64_t> until_ms;
    // The received grant a re-share was cut from.
    std::optional<GrantId> parent;
    std::int64_t created_ms = 0;
    std::optional<std::int64_t> revoked_ms;

    friend bool operator==(const IssuedRecord&, const IssuedRecord&) = default;
};

// One node's grant tables. Thread-safe: every call takes the connection lock.
class GrantStore final : public Revocations {
  public:
    // The schema text executed on open, identical to the Rust store's.
    static const std::string_view SCHEMA;

    [[nodiscard]] static Result<GrantStore> open(const std::filesystem::path& path);
    // A private in-memory database.
    [[nodiscard]] static Result<GrantStore> open_in_memory();
    // Takes ownership of an open connection, creating the tables if needed.
    [[nodiscard]] static Result<GrantStore> with_connection(sqlite3* db);

    GrantStore(GrantStore&&) noexcept;
    GrantStore& operator=(GrantStore&&) noexcept;
    ~GrantStore() override;

    // Writes one `grants_issued` row for `issued`. `issue` and `reshare` call it.
    Result<IssuedGrant> insert_issued(const IssuedGrant& issued, const Definition& definition,
                                      const GrantId* parent, std::int64_t now_ms) const;

    // Creates a grant for `holder` and mints its first token.
    [[nodiscard]] Result<IssuedGrant> issue(const NodeIdentity& issuer, const PublicKey& holder, GrantSpec spec,
                                            std::int64_t now_ms) const;
    // Mints a fresh token for an active grant this node issued, for its holder only.
    [[nodiscard]] Result<IssuedGrant> renew(const NodeIdentity& issuer, const GrantId& id, const PublicKey& requester,
                                            std::int64_t now_ms) const;
    // Moves the end of a root grant to `until_ms` (none: until revoked) and
    // mints a token at once, so the holder sees the change before its next renewal.
    [[nodiscard]] Result<IssuedGrant> extend(const NodeIdentity& issuer, const GrantId& id,
                                             std::optional<std::int64_t> until_ms, std::int64_t now_ms) const;
    // Re-shares a held grant to `new_holder`, narrowed. Works offline: the new
    // token carries a block signed by this node. The definition is stored so
    // the new holder can renew through us.
    [[nodiscard]] Result<IssuedGrant> reshare(const NodeIdentity& holder, const GrantId& parent,
                                              const PublicKey& new_holder, Narrowing narrowing,
                                              std::int64_t now_ms) const;
    // Revokes a grant this node issued: renewal is refused and this node's
    // verifier denies its tokens at once.
    Result<void> revoke(const PublicKey& issuer, const GrantId& id, std::int64_t now_ms) const;
    // Records a revocation notice from a peer, so this node denies re-shares of it at once.
    Result<void> record_revocation(const GrantId& id, const PublicKey& issuer, std::int64_t revoked_ms) const;
    // Every grant this node issued, oldest first (by `created_ms`, then id).
    [[nodiscard]] Result<std::vector<IssuedRecord>> issued() const;

    // Stores a token issued (or renewed) to `own` once its chain from `root`
    // checks out and ends at `own`. An existing row is replaced only when its
    // issuer and root match.
    Result<ReceivedGrant> receive(const PublicKey& own, const PublicKey& root, std::span<const std::uint8_t> token) const;
    // `receive`, accepting only a fresher token for `expected` from the same issuer.
    Result<ReceivedGrant> receive_renewal(const PublicKey& own, const PublicKey& root, const GrantId& expected,
                                          std::span<const std::uint8_t> token) const;
    // Marks a held grant as refused by its issuer: renewal stops trying.
    Result<void> mark_refused(const GrantId& id, std::int64_t now_ms) const;
    [[nodiscard]] Result<std::optional<ReceivedGrant>> received(const GrantId& id) const;
    // Every grant this node holds, soonest expiry first.
    [[nodiscard]] Result<std::vector<ReceivedGrant>> received_all() const;

    // A read failure counts as revoked: an unchecked token must not pass.
    [[nodiscard]] bool is_revoked(const GrantId& grant) const override;

    // The connection, for inspection; owned by the store.
    [[nodiscard]] sqlite3* connection() const noexcept;

  private:
    struct Impl;
    explicit GrantStore(std::unique_ptr<Impl> impl);

    Result<IssuedGrant> attenuate_from(const NodeIdentity& holder, const GrantId& parent, GrantId id,
                                       const PublicKey& new_holder, const Narrowing& narrowing,
                                       std::int64_t now_ms) const;
    Result<ReceivedGrant> receive_checked(const PublicKey& own, const PublicKey& root,
                                          std::span<const std::uint8_t> token, const GrantId* expected) const;

    std::unique_ptr<Impl> impl_;
};

} // namespace loa::trust
