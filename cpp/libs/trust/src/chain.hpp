#pragma once

// Token internals shared by the verifier and the grant store.

#include <cstdint>
#include <optional>
#include <span>
#include <vector>

#include "loa/biscuit_ffi/biscuit.hpp"
#include "loa/trust/token.hpp"

namespace loa::trust::detail {

// A token parsed and its chain checked, without a holder proof yet.
struct Chain {
    biscuit_ffi::Biscuit biscuit;
    std::vector<Link> links;

    // `parse` never builds a chain without the authority link.
    [[nodiscard]] const Link& last() const { return links.back(); }
    [[nodiscard]] std::int64_t expires_ms() const;

    // Verifies the signatures against `root` and walks the holder chain.
    [[nodiscard]] static std::expected<Chain, Denied> parse(std::span<const std::uint8_t> token, const PublicKey& root);
};

// Builds the original token: the scopes as right facts, an expiry check and
// the chain context, signed by `issuer`.
[[nodiscard]] Result<std::vector<std::uint8_t>> mint(const NodeIdentity& issuer, const GrantId& grant,
                                                     const PublicKey& holder, std::span<const Scope> scopes,
                                                     std::int64_t expires_ms, bool reshare);

// Appends a re-share block signed by `holder`, the chain's current last holder.
[[nodiscard]] Result<std::vector<std::uint8_t>> attenuate(const Chain& parent, const NodeIdentity& holder,
                                                          const GrantId& grant, const PublicKey& new_holder,
                                                          std::optional<std::span<const Scope>> scopes,
                                                          std::int64_t expires_ms, bool reshare);

[[nodiscard]] inline std::int64_t saturating_add(std::int64_t a, std::int64_t b) {
    std::int64_t out;
    if (__builtin_add_overflow(a, b, &out)) return b > 0 ? INT64_MAX : INT64_MIN;
    return out;
}

[[nodiscard]] inline std::int64_t saturating_sub(std::int64_t a, std::int64_t b) {
    std::int64_t out;
    if (__builtin_sub_overflow(a, b, &out)) return b < 0 ? INT64_MAX : INT64_MIN;
    return out;
}

} // namespace loa::trust::detail
