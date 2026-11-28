#include <doctest/doctest.h>

#include <array>
#include <cstdint>
#include <span>
#include <string>
#include <vector>

#include "loa/bus/writer.hpp"
#include "loa/trust/key.hpp"
#include "loa/trust/keystore.hpp"

using namespace loa::trust;

namespace {

NodeIdentity key(std::uint8_t byte) {
    std::array<std::uint8_t, 32> seed{};
    seed.fill(byte);
    return NodeIdentity::from_secret_bytes(seed);
}

std::span<const std::uint8_t> bytes_of(std::string_view s) {
    return {reinterpret_cast<const std::uint8_t*>(s.data()), s.size()};
}

} // namespace

TEST_CASE("public key prints hex and parses hex and base32, rejecting off-curve points") {
    const auto id = key(42);
    // Independently computed (Python `cryptography`) for seed [42; 32].
    const std::string hex = "197f6b23e16c8532c6abc838facd5ea789be0c76b2920334039bfa8b3d368d61";
    CHECK(id.public_key().to_string() == hex);
    CHECK(id.public_key().debug() == "PublicKey(" + hex + ")");
    CHECK(id.debug() == "NodeIdentity(" + hex + ")");
    CHECK(PublicKey::parse(hex) == id.public_key());
    std::string upper = hex;
    for (auto& c : upper) c = static_cast<char>(std::toupper(static_cast<unsigned char>(c)));
    CHECK(PublicKey::parse(upper) == id.public_key());
    CHECK(PublicKey::parse("df7wwi7bnsctfrvlza4pvtk6u6e34ddwwkjagnadtp5iwpjwrvqq") == id.public_key());
    CHECK(PublicKey::parse("DF7WWI7BNSCTFRVLZA4PVTK6U6E34DDWWKJAGNADTP5IWPJWRVQQ") == id.public_key());
    // Non-zero trailing bits are not canonical base32.
    CHECK(!PublicKey::parse("df7wwi7bnsctfrvlza4pvtk6u6e34ddwwkjagnadtp5iwpjwrvqr"));

    const auto bad = PublicKey::parse("not a key");
    REQUIRE(!bad);
    CHECK(bad.error().kind() == TrustError::Kind::key);
    CHECK(bad.error().to_string() == "invalid key: not a public key: \"NOT A KEY\"");

    // y = 2 has no x on edwards25519; y = 3 does.
    std::string off = "02" + std::string(62, '0');
    CHECK(PublicKey::from_hex(off).has_value());
    const auto rejected = PublicKey::parse(off);
    REQUIRE(!rejected);
    CHECK(rejected.error().to_string() == "invalid key: not an ed25519 point");
    CHECK(PublicKey::parse("03" + std::string(62, '0')).has_value());
}

TEST_CASE("one key for the bus; strict signatures") {
    const auto id = key(42);
    CHECK(from_bus(to_bus(id.public_key())) == id.public_key());
    CHECK(to_bus(id.public_key()) == *loa::bus::NodeKey::from_seed(id.secret_bytes()));

    const auto signature = id.sign(bytes_of("dial me"));
    CHECK(id.public_key().verify(bytes_of("dial me"), signature));
    CHECK(!id.public_key().verify(bytes_of("dial you"), signature));
    CHECK(!key(1).public_key().verify(bytes_of("dial me"), signature));

    // A malleated S (S + L) fails.
    static constexpr std::uint8_t order[32] = {0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7,
                                               0xa2, 0xde, 0xf9, 0xde, 0x14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10};
    auto malleated = signature;
    unsigned carry = 0;
    for (int i = 0; i < 32; ++i) {
        const unsigned sum = malleated[32 + i] + order[i] + carry;
        malleated[32 + i] = static_cast<std::uint8_t>(sum);
        carry = sum >> 8;
    }
    CHECK(!id.public_key().verify(bytes_of("dial me"), malleated));

    // A small order (weak) key never verifies, even with the all-zero R/S
    // signature that it would accept non-strictly.
    PublicKey identity_point{};
    identity_point.bytes[0] = 1;
    Signature trivial{};
    trivial[0] = 1;
    CHECK(!identity_point.verify(bytes_of("anything"), trivial));
}

TEST_CASE("proof binds challenge and token") {
    const auto holder = key(3);
    const auto challenge = Challenge::random();
    REQUIRE(challenge);
    const auto other = Challenge::random();
    REQUIRE(other);
    CHECK(*challenge != *other);
    const std::vector<std::uint8_t> token = {1, 2, 3};
    const auto proof = prove(holder, *challenge, token);
    CHECK(proof_valid(holder.public_key(), *challenge, token, proof));
    CHECK(!proof_valid(holder.public_key(), *other, token, proof));
    CHECK(!proof_valid(holder.public_key(), *challenge, std::vector<std::uint8_t>{1, 2}, proof));
    CHECK(!proof_valid(key(4).public_key(), *challenge, token, proof));

    // The signed message is the label, a NUL, the challenge, then the token.
    std::string message("loa-grant-proof/1\0", 18);
    message.append(reinterpret_cast<const char*>(challenge->bytes.data()), 32);
    message.append("\x01\x02\x03", 3);
    CHECK(proof.bytes == holder.sign(bytes_of(message)));
}

TEST_CASE("generate gives fresh keys; load_or_create keeps them") {
    const auto a = NodeIdentity::generate();
    const auto b = NodeIdentity::generate();
    REQUIRE(a);
    REQUIRE(b);
    CHECK(a->public_key() != b->public_key());

    const MemoryKeyStore store;
    const auto first = NodeIdentity::load_or_create(store);
    REQUIRE(first);
    const auto again = NodeIdentity::load_or_create(store);
    REQUIRE(again);
    CHECK(again->public_key() == first->public_key());
    CHECK(again->secret_bytes() == first->secret_bytes());
}

TEST_CASE("a wrong-length stored secret fails and is left unchanged") {
    const std::vector<std::uint8_t> bad = {1, 2, 3};
    const auto store = MemoryKeyStore::with_secret(bad);
    const auto loaded = NodeIdentity::load_or_create(store);
    REQUIRE(!loaded);
    CHECK(loaded.error().kind() == TrustError::Kind::wrong_length);
    CHECK(loaded.error().found() == 3);
    CHECK(loaded.error().to_string() == "stored node key has 3 bytes, expected 32");
    const auto kept = store.load();
    REQUIRE(kept);
    REQUIRE(kept->has_value());
    CHECK(std::vector<std::uint8_t>((*kept)->bytes().begin(), (*kept)->bytes().end()) == bad);
}
