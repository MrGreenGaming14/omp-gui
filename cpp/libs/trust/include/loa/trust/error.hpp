#pragma once

// Everything that can go wrong with keys and grants (Rust `loa_trust::TrustError`).
// No error carries secret bytes, so all are safe to log.

#include <cstddef>
#include <cstdint>
#include <expected>
#include <string>

#include "loa/runtime/error.hpp"

namespace loa::trust {

// Length of the secret seed, which is all that is stored.
inline constexpr std::size_t SECRET_KEY_LENGTH = 32;

class TrustError {
  public:
    enum class Kind {
        store,             // the key store failed
        unavailable,       // the OS credential store can't be used at all; a FallbackStore moves on
        wrong_length,      // the store holds a value that is not a 32 byte seed; it is left in place
        random,            // the OS random source failed
        key,               // an invalid key
        sql,               // the grant database failed
        json,              // a grant definition doesn't encode or decode
        biscuit,           // the Biscuit library refused
        action,            // `detail()` grants don't support action `message()`
        no_scopes,         // a grant needs at least one scope
        no_actions,        // a scope needs at least one action
        ttl,               // token lifetime `number()` is not positive
        node_id,           // node id `message()` doesn't fit a grant
        folder_path,       // folder path `message()` isn't normalized and absolute
        unknown_grant,     // grant `message()` is unknown
        revoked,           // grant `message()` is revoked
        expired,           // grant `message()` has ended
        not_holder,        // the requester doesn't hold grant `message()`
        reshare_forbidden, // grant `message()` may not be re-shared
        parent_expired,    // the re-shared-from grant `message()` has no current token
        malformed,         // a malformed grant token
    };

    [[nodiscard]] static TrustError store(std::string m) { return {Kind::store, std::move(m)}; }
    [[nodiscard]] static TrustError unavailable(std::string m) { return {Kind::unavailable, std::move(m)}; }
    [[nodiscard]] static TrustError wrong_length(std::size_t found) { return {Kind::wrong_length, {}, found}; }
    [[nodiscard]] static TrustError random(std::string m) { return {Kind::random, std::move(m)}; }
    [[nodiscard]] static TrustError key(std::string m) { return {Kind::key, std::move(m)}; }
    [[nodiscard]] static TrustError sql(std::string m) { return {Kind::sql, std::move(m)}; }
    [[nodiscard]] static TrustError json(std::string m) { return {Kind::json, std::move(m)}; }
    [[nodiscard]] static TrustError biscuit(std::string m) { return {Kind::biscuit, std::move(m)}; }
    // `resource` is the kind name, `action` the action's text.
    [[nodiscard]] static TrustError action(std::string resource, std::string action) {
        TrustError e{Kind::action, std::move(action)};
        e.detail_ = std::move(resource);
        return e;
    }
    [[nodiscard]] static TrustError no_scopes() { return {Kind::no_scopes, {}}; }
    [[nodiscard]] static TrustError no_actions() { return {Kind::no_actions, {}}; }
    [[nodiscard]] static TrustError ttl(std::int64_t ttl_ms) {
        TrustError e{Kind::ttl, {}};
        e.number_ = ttl_ms;
        return e;
    }
    [[nodiscard]] static TrustError node_id(std::uint64_t id) { return {Kind::node_id, std::to_string(id)}; }
    [[nodiscard]] static TrustError folder_path(std::string path) { return {Kind::folder_path, std::move(path)}; }
    // The grant variants carry the grant id.
    [[nodiscard]] static TrustError unknown_grant(std::string id) { return {Kind::unknown_grant, std::move(id)}; }
    [[nodiscard]] static TrustError revoked(std::string id) { return {Kind::revoked, std::move(id)}; }
    [[nodiscard]] static TrustError expired(std::string id) { return {Kind::expired, std::move(id)}; }
    [[nodiscard]] static TrustError not_holder(std::string id) { return {Kind::not_holder, std::move(id)}; }
    [[nodiscard]] static TrustError reshare_forbidden(std::string id) { return {Kind::reshare_forbidden, std::move(id)}; }
    [[nodiscard]] static TrustError parent_expired(std::string id) { return {Kind::parent_expired, std::move(id)}; }
    [[nodiscard]] static TrustError malformed(std::string m) { return {Kind::malformed, std::move(m)}; }

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The variant's payload text: a message, grant id, path or action (empty when none).
    [[nodiscard]] const std::string& message() const noexcept { return message_; }
    // The resource kind name for `action`.
    [[nodiscard]] const std::string& detail() const noexcept { return detail_; }
    // The stored length for `wrong_length`.
    [[nodiscard]] std::size_t found() const noexcept { return found_; }
    // The lifetime for `ttl`.
    [[nodiscard]] std::int64_t number() const noexcept { return number_; }

    // Rust `Display`, e.g. `os keychain unavailable: …`.
    [[nodiscard]] std::string to_string() const;
    [[nodiscard]] loa::Error to_error() const;

    friend bool operator==(const TrustError&, const TrustError&) = default;

  private:
    TrustError(Kind kind, std::string message, std::size_t found = 0)
        : kind_(kind), message_(std::move(message)), found_(found) {}

    Kind kind_;
    std::string message_;
    std::string detail_;
    std::size_t found_;
    std::int64_t number_ = 0;
};

template <class T>
using Result = std::expected<T, TrustError>;

} // namespace loa::trust
