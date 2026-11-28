#pragma once

// Errors of key handling (Rust `loa_trust::TrustError`, identity variants). No
// error carries secret bytes, so all are safe to log.

#include <cstddef>
#include <expected>
#include <string>

#include "loa/runtime/error.hpp"

namespace loa::trust {

// Length of the secret seed, which is all that is stored.
inline constexpr std::size_t SECRET_KEY_LENGTH = 32;

class TrustError {
  public:
    enum class Kind {
        store,        // the key store failed
        unavailable,  // the OS credential store can't be used at all; a FallbackStore moves on
        wrong_length, // the store holds a value that is not a 32 byte seed; it is left in place
        random,       // the OS random source failed
        key,          // an invalid key
    };

    [[nodiscard]] static TrustError store(std::string m) { return {Kind::store, std::move(m)}; }
    [[nodiscard]] static TrustError unavailable(std::string m) { return {Kind::unavailable, std::move(m)}; }
    [[nodiscard]] static TrustError wrong_length(std::size_t found) { return {Kind::wrong_length, {}, found}; }
    [[nodiscard]] static TrustError random(std::string m) { return {Kind::random, std::move(m)}; }
    [[nodiscard]] static TrustError key(std::string m) { return {Kind::key, std::move(m)}; }

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // The variant's payload text (empty for `wrong_length`).
    [[nodiscard]] const std::string& message() const noexcept { return message_; }
    // The stored length for `wrong_length`.
    [[nodiscard]] std::size_t found() const noexcept { return found_; }

    // Rust `Display`, e.g. `os keychain unavailable: …`.
    [[nodiscard]] std::string to_string() const;
    [[nodiscard]] loa::Error to_error() const;

    friend bool operator==(const TrustError&, const TrustError&) = default;

  private:
    TrustError(Kind kind, std::string message, std::size_t found = 0)
        : kind_(kind), message_(std::move(message)), found_(found) {}

    Kind kind_;
    std::string message_;
    std::size_t found_;
};

template <class T>
using Result = std::expected<T, TrustError>;

} // namespace loa::trust
