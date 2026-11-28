#pragma once

// Ed25519 signatures over the SHA-256 digest of release artifacts. A release ships
// `<artifact>.sig`: the 64-byte signature as hex text, optionally newline-terminated.

#include <array>
#include <cstdint>
#include <optional>
#include <string>
#include <span>
#include <string_view>
#include <vector>

#include "loa/updater/error.hpp"

namespace loa::updater {

struct Staged;

// A 32-byte Ed25519 public key.
struct VerifyingKey {
    std::array<std::uint8_t, 32> bytes{};
    friend bool operator==(const VerifyingKey&, const VerifyingKey&) = default;
};

namespace signature {

// SHA-256 of the file at `path`.
[[nodiscard]] Result<std::array<std::uint8_t, 32>> artifact_digest(const Path& path);

// Ed25519 `verify_strict` of `signature` over `message`: canonical scalar, no small-order key or R.
[[nodiscard]] bool verify_strict(const VerifyingKey& key, std::span<const std::uint8_t> message, std::span<const std::uint8_t, 64> signature);

// Re-hashes the staged artifact against its manifest and checks its signature by one of `trusted`.
[[nodiscard]] Result<> verify_staged(const Staged& staged, std::span<const VerifyingKey> trusted);

// A 32-byte Ed25519 public key from hex.
[[nodiscard]] Result<VerifyingKey> parse_public_key(std::string_view key_hex);

} // namespace signature

// Lowercase hex.
[[nodiscard]] std::string to_hex(std::span<const std::uint8_t> bytes);
// Hex (either case) to bytes; none when malformed.
[[nodiscard]] std::optional<std::vector<std::uint8_t>> from_hex(std::string_view text);

} // namespace loa::updater
