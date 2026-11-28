#pragma once

// Grant tokens: minting, attenuating, verifying and authorizing. The Datalog is
// generated from `KINDS`.
//
// The authority block, signed by the issuer, holds the rights as facts and an
// expiry check on `time_ms`. Every re-share appends a third party block signed
// by the previous holder's key that holds only checks: the narrowed scopes and
// its own expiry. Each block's context is JSON `{g, h, e, r}`: grant id, holder
// (hex), expiry (unix ms) and whether that holder may re-share.

#include <compare>
#include <cstddef>
#include <cstdint>
#include <expected>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <vector>

#include "loa/trust/error.hpp"
#include "loa/trust/key.hpp"
#include "loa/trust/scope.hpp"

namespace loa::trust {

// Name of the app's MCP server for SSH tools.
inline constexpr std::string_view REMOTE_SERVER = "remote";

// Default lifetime of a token. Grants renew well before this runs out.
inline constexpr std::int64_t DEFAULT_TTL_MS = 10 * 60'000;

// The authority and re-share blocks' expiry check; `{expires}` is a parameter.
inline constexpr std::string_view EXPIRY_CHECK = "check if time_ms($t), $t < {expires};";

// Id of one grant definition. Stays the same across renewals.
struct GrantId {
    std::string value;

    // 16 bytes from the OS random source, as lowercase hex.
    [[nodiscard]] static GrantId random();
    [[nodiscard]] const std::string& as_str() const noexcept { return value; }

    friend bool operator==(const GrantId&, const GrantId&) = default;
    friend auto operator<=>(const GrantId&, const GrantId&) = default;
};

// One holder in a token's chain: the original grant (index 0) or a re-share.
struct Link {
    GrantId grant;
    // Who this block was issued to.
    PublicKey holder;
    // Who signed it: the root for index 0, the previous holder for a re-share.
    PublicKey issuer;
    std::int64_t expires_ms = 0;
    // Whether `holder` may re-share.
    bool reshare = false;

    friend bool operator==(const Link&, const Link&) = default;
};

// Why a token or a request was refused.
struct Denied {
    enum class Kind {
        bad_token,         // doesn't parse or isn't signed by this node (`message`)
        broken_chain,      // `block` isn't signed by the holder before it
        reshare_forbidden, // `block` re-shares a grant that may not be re-shared
        not_holder,        // issued to `holder`, not to `requester`
        bad_proof,         // the holder's proof doesn't match the challenge
        revoked,           // `grant` is revoked
        expired,           // expired at `expired_ms`
        not_granted,       // no `action` right on this `resource`
        narrowed,          // re-share `block` narrowed `action` on this `resource` away
        engine,            // authorization failed (`message`)
    };

    Kind kind = Kind::engine;
    std::string message;
    std::size_t block = 0;
    PublicKey holder{};
    PublicKey requester{};
    GrantId grant{};
    std::int64_t expired_ms = 0;
    Action action = Action::view;
    std::string_view resource;

    [[nodiscard]] static Denied bad_token(std::string m) { return {.kind = Kind::bad_token, .message = std::move(m)}; }
    [[nodiscard]] static Denied broken_chain(std::size_t block) { return {.kind = Kind::broken_chain, .block = block}; }
    [[nodiscard]] static Denied reshare_forbidden(std::size_t block) { return {.kind = Kind::reshare_forbidden, .block = block}; }
    [[nodiscard]] static Denied not_holder(PublicKey holder, PublicKey requester) {
        return {.kind = Kind::not_holder, .holder = holder, .requester = requester};
    }
    [[nodiscard]] static Denied bad_proof() { return {.kind = Kind::bad_proof}; }
    [[nodiscard]] static Denied revoked(GrantId grant) { return {.kind = Kind::revoked, .grant = std::move(grant)}; }
    [[nodiscard]] static Denied expired(std::int64_t at) { return {.kind = Kind::expired, .expired_ms = at}; }
    [[nodiscard]] static Denied not_granted(Action action, std::string_view resource) {
        return {.kind = Kind::not_granted, .action = action, .resource = resource};
    }
    [[nodiscard]] static Denied narrowed(Action action, std::string_view resource, std::size_t block) {
        return {.kind = Kind::narrowed, .block = block, .action = action, .resource = resource};
    }
    [[nodiscard]] static Denied engine(std::string m) { return {.kind = Kind::engine, .message = std::move(m)}; }

    // Rust `Display`.
    [[nodiscard]] std::string to_string() const;

    friend bool operator==(const Denied&, const Denied&) = default;
};

// Grant ids this node knows to be revoked.
class Revocations {
  public:
    virtual ~Revocations() = default;
    [[nodiscard]] virtual bool is_revoked(const GrantId& grant) const = 0;
};

// A token presented by a peer, with its proof over the verifier's challenge.
struct Presented {
    std::span<const std::uint8_t> token;
    const Proof* proof = nullptr;
};

namespace detail {
struct Chain;
}

// A token whose holder proved possession. Answers requests until it expires.
class VerifiedGrant {
  public:
    // The holders from the original grant to the presenter.
    [[nodiscard]] const std::vector<Link>& chain() const noexcept;
    // When the token stops working: the earliest expiry in the chain.
    [[nodiscard]] std::int64_t expires_ms() const noexcept;
    // Allows or denies `request` at `now_ms`. Runs with 1000 facts, 100
    // iterations and 250 ms at most.
    [[nodiscard]] std::expected<void, Denied> authorize(const Request& request, std::int64_t now_ms) const;

    explicit VerifiedGrant(std::shared_ptr<const detail::Chain> chain) : chain_(std::move(chain)) {}

  private:
    [[nodiscard]] std::expected<void, Denied> live(std::int64_t now_ms) const;
    std::shared_ptr<const detail::Chain> chain_;
};

// Checks tokens presented to the node that issued them (the root). Keeps a
// reference to `revocations`, which must outlive it.
class Verifier {
  public:
    Verifier(PublicKey root, const Revocations& revocations) : root_(root), revocations_(&revocations) {}

    // Checks signatures, the holder chain, that `requester` is the last holder
    // and signed `challenge`, revocation and expiry. Once per connection; then
    // `VerifiedGrant::authorize` per request.
    [[nodiscard]] std::expected<VerifiedGrant, Denied> verify(Presented presented, const PublicKey& requester,
                                                              const Challenge& challenge, std::int64_t now_ms) const;

  private:
    PublicKey root_;
    const Revocations* revocations_;
};

// The rights a token's root signed into its first block, as scopes. Re-share
// blocks only narrow these and are not reflected. For display; authorization
// goes through `Verifier`.
[[nodiscard]] Result<std::vector<Scope>> token_scopes(std::span<const std::uint8_t> token, const PublicKey& root);

// The verifier's rules: which request facts each kind of right covers.
[[nodiscard]] const std::string& rules();

} // namespace loa::trust
