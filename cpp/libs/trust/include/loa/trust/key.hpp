#pragma once

// The node keypair, its public key and the holder proof.

#include <array>
#include <compare>
#include <cstddef>
#include <cstdint>
#include <span>
#include <string>
#include <string_view>

#include "loa/trust/error.hpp"

namespace loa::bus {
struct NodeKey;
}

namespace loa::trust {

class KeyStore;

// A node's ed25519 public key: its endpoint id and grant identity. Prints as
// iroh's endpoint id (64 lowercase hex); parses that or iroh's base32 form.
struct PublicKey {
    std::array<std::uint8_t, 32> bytes{};

    [[nodiscard]] const std::array<std::uint8_t, 32>& as_bytes() const noexcept { return bytes; }

    // The key from 64 hex characters (either case), as stored; no curve check.
    [[nodiscard]] static Result<PublicKey> from_hex(std::string_view text);
    // An endpoint id: 64 hex characters, else unpadded base32 (any case). Fails
    // unless the bytes are an ed25519 curve point (Rust `FromStr`).
    [[nodiscard]] static Result<PublicKey> parse(std::string_view text);

    // True when `signature` is this key's over `message`. Strict, as in Biscuit:
    // weak (small order) keys, small order R and malleable signatures fail.
    [[nodiscard]] bool verify(std::span<const std::uint8_t> message, std::span<const std::uint8_t, 64> signature) const;

    // 64 lowercase hex (Rust `Display`).
    [[nodiscard]] std::string to_string() const;
    // `PublicKey(<hex>)` (Rust `Debug`).
    [[nodiscard]] std::string debug() const;

    friend bool operator==(const PublicKey&, const PublicKey&) = default;
    friend auto operator<=>(const PublicKey&, const PublicKey&) = default;
};

// The same 32 bytes as the session bus key.
[[nodiscard]] PublicKey from_bus(const bus::NodeKey& key);
[[nodiscard]] bus::NodeKey to_bus(const PublicKey& key);

using Signature = std::array<std::uint8_t, 64>;

// This node's ed25519 keypair: its network address and signing key. The
// secret is wiped on destruction and never printed.
class NodeIdentity {
  public:
    // Loads the key from `store`, creating and storing one on first run. A
    // stored value of the wrong length is an error and is never replaced.
    [[nodiscard]] static Result<NodeIdentity> load_or_create(const KeyStore& store);
    // A fresh key from the OS random source (RAND_bytes), stored nowhere.
    [[nodiscard]] static Result<NodeIdentity> generate();
    [[nodiscard]] static NodeIdentity from_secret_bytes(std::span<const std::uint8_t, SECRET_KEY_LENGTH> seed);

    NodeIdentity(const NodeIdentity&) = default;
    NodeIdentity& operator=(const NodeIdentity&) = default;
    ~NodeIdentity();

    // The seed; the network layer builds its iroh secret key from it.
    [[nodiscard]] const std::array<std::uint8_t, SECRET_KEY_LENGTH>& secret_bytes() const noexcept { return seed_; }
    [[nodiscard]] const PublicKey& public_key() const noexcept { return public_; }
    // Signs `message` (pure Ed25519, RFC 8032).
    [[nodiscard]] Signature sign(std::span<const std::uint8_t> message) const;

    // `NodeIdentity(<public hex>)`; never the secret.
    [[nodiscard]] std::string debug() const;

  private:
    NodeIdentity() = default;

    std::array<std::uint8_t, SECRET_KEY_LENGTH> seed_{};
    PublicKey public_;
};

// The seed in `secret`, or `wrong_length`.
[[nodiscard]] Result<std::array<std::uint8_t, SECRET_KEY_LENGTH>> seed_of(std::span<const std::uint8_t> secret);

// A fresh nonce per connection the holder signs, so proofs can't be replayed.
struct Challenge {
    std::array<std::uint8_t, 32> bytes{};

    [[nodiscard]] static Result<Challenge> random();

    friend bool operator==(const Challenge&, const Challenge&) = default;
};

// The holder's signature over a challenge and the token it presents.
struct Proof {
    Signature bytes{};

    friend bool operator==(const Proof&, const Proof&) = default;
};

// Signs `loa-grant-proof/1`, NUL, `challenge`, then `token`, binding both so the
// proof can't be reused.
[[nodiscard]] Proof prove(const NodeIdentity& holder, const Challenge& challenge, std::span<const std::uint8_t> token);
[[nodiscard]] bool proof_valid(const PublicKey& holder, const Challenge& challenge, std::span<const std::uint8_t> token,
                               const Proof& proof);

} // namespace loa::trust
