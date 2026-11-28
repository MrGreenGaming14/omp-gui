#pragma once

// Holder side renewal: refresh short lived tokens before they run out.

#include <cstdint>
#include <expected>
#include <optional>
#include <string>
#include <variant>
#include <vector>

#include "loa/trust/store.hpp"

// Forward declarations only: implementations include <asio.hpp> themselves.
namespace asio {
class any_io_executor;
template <typename T, typename Executor> class awaitable;
} // namespace asio

namespace loa::trust {

template <typename T>
using Awaitable = asio::awaitable<T, asio::any_io_executor>;

// Why an issuer didn't hand out a fresh token.
struct RenewFailure {
    enum class Kind {
        refused,     // revoked, unknown, ended or not ours; final
        unreachable, // unreachable or failed; retried next round
    };
    Kind kind = Kind::unreachable;
    std::string reason;

    [[nodiscard]] static RenewFailure refused(std::string reason) { return {Kind::refused, std::move(reason)}; }
    [[nodiscard]] static RenewFailure unreachable(std::string reason) { return {Kind::unreachable, std::move(reason)}; }
    // How the issuer side reports a failed `GrantStore::renew`.
    [[nodiscard]] static RenewFailure from_issuer(const TrustError& error);

    // Rust `Display`.
    [[nodiscard]] std::string to_string() const;

    friend bool operator==(const RenewFailure&, const RenewFailure&) = default;
};

// Asks a grant's issuer for a fresh token (over `omp/control/1`).
class RenewTransport {
  public:
    virtual ~RenewTransport() = default;
    [[nodiscard]] virtual Awaitable<std::expected<std::vector<std::uint8_t>, RenewFailure>> renew(PublicKey issuer,
                                                                                                    GrantId grant) const = 0;
};

// What happened to one grant in a renewal round.
namespace renew_outcome {
struct Renewed {
    GrantId grant;
    std::int64_t expires_ms = 0;
    friend bool operator==(const Renewed&, const Renewed&) = default;
};
// The issuer refused; the grant is marked and not asked again.
struct Refused {
    GrantId grant;
    std::string reason;
    friend bool operator==(const Refused&, const Refused&) = default;
};
// Try again next round.
struct Failed {
    GrantId grant;
    std::string reason;
    friend bool operator==(const Failed&, const Failed&) = default;
};
} // namespace renew_outcome

using RenewOutcome = std::variant<renew_outcome::Renewed, renew_outcome::Refused, renew_outcome::Failed>;

// Keeps held grants fresh: call `renew_due` at `next_due`.
class Renewer {
  public:
    // Renews `margin_ms` before a token expires.
    Renewer(PublicKey own, std::int64_t margin_ms) : own_(own), margin_ms_(margin_ms) {}

    // When the next renewal is due; none when nothing needs renewing.
    [[nodiscard]] Result<std::optional<std::int64_t>> next_due(const GrantStore& store) const;
    // Renews every held grant due at `now_ms` through `transport`. `store` and
    // `transport` must outlive the coroutine.
    [[nodiscard]] Awaitable<Result<std::vector<RenewOutcome>>> renew_due(const GrantStore& store,
                                                                          const RenewTransport& transport,
                                                                          std::int64_t now_ms) const;

  private:
    [[nodiscard]] std::optional<std::int64_t> due_at(const ReceivedGrant& grant) const;

    PublicKey own_;
    std::int64_t margin_ms_;
};

} // namespace loa::trust
