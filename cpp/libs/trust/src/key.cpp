#include "loa/trust/key.hpp"

#include <openssl/bn.h>
#include <openssl/crypto.h>
#include <openssl/err.h>
#include <openssl/rand.h>

#include <algorithm>
#include <cstdlib>
#include <format>
#include <memory>
#include <optional>
#include <vector>

#include "loa/bus/writer.hpp"
#include "loa/trust/keystore.hpp"

namespace loa::trust {
namespace {

std::string openssl_error() {
    const auto code = ERR_get_error();
    if (code == 0) return "unknown error";
    char buf[256];
    ERR_error_string_n(code, buf, sizeof buf);
    return buf;
}

Result<void> fill_random(std::span<std::uint8_t> out) {
    if (RAND_bytes(out.data(), static_cast<int>(out.size())) != 1) return std::unexpected(TrustError::random(openssl_error()));
    return {};
}

// Rust `{text:?}` for the error text: quoted, with quotes and backslashes escaped.
std::string quoted(std::string_view text) {
    std::string out = "\"";
    for (const char c : text) {
        if (c == '"' || c == '\\') out += '\\';
        out += c;
    }
    return out + '"';
}

TrustError not_a_key(std::string_view text) { return TrustError::key("not a public key: " + quoted(text)); }

int hex_digit(char c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

// data-encoding BASE32_NOPAD: RFC 4648 alphabet, no padding, trailing bits zero.
std::optional<std::vector<std::uint8_t>> base32_decode(std::string_view text) {
    switch (text.size() % 8) {
        case 1: case 3: case 6: return std::nullopt;
        default: break;
    }
    std::vector<std::uint8_t> out;
    std::uint32_t acc = 0;
    int bits = 0;
    for (const char c : text) {
        int v;
        if (c >= 'A' && c <= 'Z') v = c - 'A';
        else if (c >= '2' && c <= '7') v = c - '2' + 26;
        else return std::nullopt;
        acc = (acc << 5) | static_cast<std::uint32_t>(v);
        bits += 5;
        if (bits >= 8) {
            bits -= 8;
            out.push_back(static_cast<std::uint8_t>(acc >> bits));
            acc &= (1u << bits) - 1;
        }
    }
    if (acc != 0) return std::nullopt;
    return out;
}

using Bn = std::unique_ptr<BIGNUM, decltype(&BN_free)>;
Bn bn() { return Bn(BN_new(), &BN_free); }

// True when `bytes` decompresses to a point on edwards25519, as
// curve25519-dalek's `CompressedEdwardsY::decompress`: y is taken mod p with
// the sign bit cleared, and x² = (y² - 1) / (d y² + 1) must have a root.
bool on_curve(const std::array<std::uint8_t, 32>& bytes) {
    std::array<std::uint8_t, 32> le = bytes;
    le[31] &= 0x7f;
    std::unique_ptr<BN_CTX, decltype(&BN_CTX_free)> ctx(BN_CTX_new(), &BN_CTX_free);
    Bn p = bn(), d = bn(), y = bn(), y2 = bn(), u = bn(), v = bn(), w = bn(), e = bn(), t = bn(), one = bn();
    if (!ctx || !p || !d || !y || !y2 || !u || !v || !w || !e || !t || !one) return false;
    // p = 2^255 - 19; d = -121665 / 121666 mod p.
    BN_set_bit(p.get(), 255);
    BN_sub_word(p.get(), 19);
    BN_one(one.get());
    BN_set_word(t.get(), 121666);
    if (!BN_mod_inverse(t.get(), t.get(), p.get(), ctx.get())) return false;
    BN_set_word(d.get(), 121665);
    BN_sub(d.get(), p.get(), d.get());
    BN_mod_mul(d.get(), d.get(), t.get(), p.get(), ctx.get());
    if (!BN_lebin2bn(le.data(), static_cast<int>(le.size()), y.get())) return false;
    BN_nnmod(y.get(), y.get(), p.get(), ctx.get());
    BN_mod_sqr(y2.get(), y.get(), p.get(), ctx.get());
    BN_mod_sub(u.get(), y2.get(), one.get(), p.get(), ctx.get());
    BN_mod_mul(v.get(), d.get(), y2.get(), p.get(), ctx.get());
    BN_mod_add(v.get(), v.get(), one.get(), p.get(), ctx.get());
    if (BN_is_zero(u.get())) return true;
    if (!BN_mod_inverse(t.get(), v.get(), p.get(), ctx.get())) return false;
    BN_mod_mul(w.get(), u.get(), t.get(), p.get(), ctx.get());
    // Euler's criterion: w^((p-1)/2) == 1.
    BN_sub(e.get(), p.get(), one.get());
    BN_rshift1(e.get(), e.get());
    BN_mod_exp(t.get(), w.get(), e.get(), p.get(), ctx.get());
    return BN_is_one(t.get());
}

// The encodings of the small order points, canonical or not, compared with the
// sign bit cleared (libsodium's blocklist). `verify_strict` rejects these as
// key or R.
bool small_order(std::span<const std::uint8_t, 32> point) {
    static constexpr std::uint8_t blocked[7][32] = {
        {0},
        {1},
        {0x26, 0xe8, 0x95, 0x8f, 0xc2, 0xb2, 0x27, 0xb0, 0x45, 0xc3, 0xf4, 0x89, 0xf2, 0xef, 0x98, 0xf0,
         0xd5, 0xdf, 0xac, 0x05, 0xd3, 0xc6, 0x33, 0x39, 0xb1, 0x38, 0x02, 0x88, 0x6d, 0x53, 0xfc, 0x05},
        {0xc7, 0x17, 0x6a, 0x70, 0x3d, 0x4d, 0xd8, 0x4f, 0xba, 0x3c, 0x0b, 0x76, 0x0d, 0x10, 0x67, 0x0f,
         0x2a, 0x20, 0x53, 0xfa, 0x2c, 0x39, 0xcc, 0xc6, 0x4e, 0xc7, 0xfd, 0x77, 0x92, 0xac, 0x03, 0x7a},
        {0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
         0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f},
        {0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
         0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f},
        {0xee, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
         0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f},
    };
    for (const auto& b : blocked) {
        if (std::equal(point.begin(), point.begin() + 31, b) && (point[31] & 0x7f) == b[31]) return true;
    }
    return false;
}

std::vector<std::uint8_t> proof_message(const Challenge& challenge, std::span<const std::uint8_t> token) {
    static constexpr std::string_view label{"loa-grant-proof/1\0", 18};
    std::vector<std::uint8_t> out(label.begin(), label.end());
    out.insert(out.end(), challenge.bytes.begin(), challenge.bytes.end());
    out.insert(out.end(), token.begin(), token.end());
    return out;
}

} // namespace

Result<PublicKey> PublicKey::from_hex(std::string_view text) {
    if (text.size() != 64) return std::unexpected(not_a_key(text));
    PublicKey key;
    for (std::size_t i = 0; i < 32; ++i) {
        const int hi = hex_digit(text[2 * i]), lo = hex_digit(text[2 * i + 1]);
        if (hi < 0 || lo < 0) return std::unexpected(not_a_key(text));
        key.bytes[i] = static_cast<std::uint8_t>(hi << 4 | lo);
    }
    return key;
}

Result<PublicKey> PublicKey::parse(std::string_view text) {
    PublicKey key;
    if (text.size() == 64) {
        auto hex = from_hex(text);
        if (!hex) return hex;
        key = *hex;
    } else {
        std::string upper(text);
        std::ranges::transform(upper, upper.begin(), [](char c) { return c >= 'a' && c <= 'z' ? static_cast<char>(c - 32) : c; });
        auto bytes = base32_decode(upper);
        if (!bytes || bytes->size() != 32) return std::unexpected(not_a_key(upper));
        std::ranges::copy(*bytes, key.bytes.begin());
    }
    if (!on_curve(key.bytes)) return std::unexpected(TrustError::key("not an ed25519 point"));
    return key;
}

bool PublicKey::verify(std::span<const std::uint8_t> message, std::span<const std::uint8_t, 64> signature) const {
    if (!on_curve(bytes) || small_order(bytes) || small_order(signature.first<32>())) return false;
    const auto ok = to_bus(*this).verify(message, signature);
    return ok && *ok;
}

std::string PublicKey::to_string() const {
    static constexpr char digits[] = "0123456789abcdef";
    std::string out;
    out.reserve(64);
    for (const auto b : bytes) {
        out += digits[b >> 4];
        out += digits[b & 15];
    }
    return out;
}

std::string PublicKey::debug() const { return "PublicKey(" + to_string() + ")"; }

PublicKey from_bus(const bus::NodeKey& key) { return PublicKey{key.bytes}; }
bus::NodeKey to_bus(const PublicKey& key) { return bus::NodeKey{key.bytes}; }

Result<NodeIdentity> NodeIdentity::load_or_create(const KeyStore& store) {
    auto stored = store.load();
    if (!stored) return std::unexpected(stored.error());
    if (*stored) {
        auto seed = seed_of((*stored)->bytes());
        if (!seed) return std::unexpected(seed.error());
        auto identity = from_secret_bytes(*seed);
        OPENSSL_cleanse(seed->data(), seed->size());
        return identity;
    }
    auto identity = generate();
    if (!identity) return identity;
    if (auto r = store.store(identity->secret_bytes()); !r) return std::unexpected(r.error());
    return identity;
}

Result<NodeIdentity> NodeIdentity::generate() {
    std::array<std::uint8_t, SECRET_KEY_LENGTH> seed{};
    if (auto r = fill_random(seed); !r) return std::unexpected(r.error());
    auto identity = from_secret_bytes(seed);
    OPENSSL_cleanse(seed.data(), seed.size());
    return identity;
}

NodeIdentity NodeIdentity::from_secret_bytes(std::span<const std::uint8_t, SECRET_KEY_LENGTH> seed) {
    NodeIdentity identity;
    std::ranges::copy(seed, identity.seed_.begin());
    // Deriving an ed25519 public key fails only when OpenSSL can't allocate.
    auto key = bus::NodeKey::from_seed(seed);
    if (!key) std::abort();
    identity.public_ = from_bus(*key);
    return identity;
}

NodeIdentity::~NodeIdentity() { OPENSSL_cleanse(seed_.data(), seed_.size()); }

Signature NodeIdentity::sign(std::span<const std::uint8_t> message) const {
    // Signing fails only when OpenSSL can't allocate.
    auto signature = bus::NodeKey::sign(seed_, message);
    if (!signature) std::abort();
    return *signature;
}

std::string NodeIdentity::debug() const { return "NodeIdentity(" + public_.to_string() + ")"; }

Result<std::array<std::uint8_t, SECRET_KEY_LENGTH>> seed_of(std::span<const std::uint8_t> secret) {
    if (secret.size() != SECRET_KEY_LENGTH) return std::unexpected(TrustError::wrong_length(secret.size()));
    std::array<std::uint8_t, SECRET_KEY_LENGTH> seed{};
    std::ranges::copy(secret, seed.begin());
    return seed;
}

Result<Challenge> Challenge::random() {
    Challenge challenge;
    if (auto r = fill_random(challenge.bytes); !r) return std::unexpected(r.error());
    return challenge;
}

Proof prove(const NodeIdentity& holder, const Challenge& challenge, std::span<const std::uint8_t> token) {
    return Proof{holder.sign(proof_message(challenge, token))};
}

bool proof_valid(const PublicKey& holder, const Challenge& challenge, std::span<const std::uint8_t> token,
                 const Proof& proof) {
    return holder.verify(proof_message(challenge, token), proof.bytes);
}

} // namespace loa::trust
