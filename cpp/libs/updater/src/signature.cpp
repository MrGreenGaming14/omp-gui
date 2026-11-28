#include "loa/updater/signature.hpp"

#include <openssl/evp.h>

#include <algorithm>
#include <cerrno>
#include <cstring>
#include <fstream>
#include <memory>

#include "loa/updater/stage.hpp"

namespace loa::updater {

std::string to_hex(std::span<const std::uint8_t> bytes) {
    static constexpr char digits[] = "0123456789abcdef";
    std::string out;
    out.reserve(bytes.size() * 2);
    for (const auto b : bytes) {
        out += digits[b >> 4];
        out += digits[b & 15];
    }
    return out;
}

std::optional<std::vector<std::uint8_t>> from_hex(std::string_view text) {
    const auto digit = [](char c) -> int {
        if (c >= '0' && c <= '9') return c - '0';
        if (c >= 'a' && c <= 'f') return c - 'a' + 10;
        if (c >= 'A' && c <= 'F') return c - 'A' + 10;
        return -1;
    };
    if (text.size() % 2) return std::nullopt;
    std::vector<std::uint8_t> out;
    out.reserve(text.size() / 2);
    for (std::size_t i = 0; i < text.size(); i += 2) {
        const int hi = digit(text[i]), lo = digit(text[i + 1]);
        if (hi < 0 || lo < 0) return std::nullopt;
        out.push_back(static_cast<std::uint8_t>(hi << 4 | lo));
    }
    return out;
}

namespace signature {
namespace {

// Encodings of the small-order points of edwards25519 (curve25519-dalek's
// EIGHT_TORSION), sign bit cleared where the comparison masks it. The final
// two encodings cover the noncanonical y values that decompress to torsion.
constexpr std::uint8_t SMALL_ORDER[7][32] = {
    // y = 0 (order 4)
    {0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
     0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00},
    // 1 (order 1)
    {0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
     0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00},
    // 2707385501144840649318225287225658788936804267575313519463743609750303402022 (order 8)
    {0x26, 0xe8, 0x95, 0x8f, 0xc2, 0xb2, 0x27, 0xb0, 0x45, 0xc3, 0xf4, 0x89, 0xf2, 0xef, 0x98, 0xf0,
     0xd5, 0xdf, 0xac, 0x05, 0xd3, 0xc6, 0x33, 0x39, 0xb1, 0x38, 0x02, 0x88, 0x6d, 0x53, 0xfc, 0x05},
    // 55188659117513257062467267217118295137698188065244968500265048394206261417927 (order 8)
    {0xc7, 0x17, 0x6a, 0x70, 0x3d, 0x4d, 0xd8, 0x4f, 0xba, 0x3c, 0x0b, 0x76, 0x0d, 0x10, 0x67, 0x0f,
     0x2a, 0x20, 0x53, 0xfa, 0x2c, 0x39, 0xcc, 0xc6, 0x4e, 0xc7, 0xfd, 0x77, 0x92, 0xac, 0x03, 0x7a},
    // p-1 (order 2)
    {0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
     0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f},
    // p (=0, order 4)
    {0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
     0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f},
    // p+1 (=1, order 1)
    {0xee, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
     0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f},
};

bool has_small_order(const std::uint8_t* s) {
    for (const auto& point : SMALL_ORDER) {
        bool equal = std::memcmp(point, s, 31) == 0 && point[31] == (s[31] & 0x7f);
        if (equal) return true;
    }
    return false;
}

using PKey = std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)>;
using MdCtx = std::unique_ptr<EVP_MD_CTX, decltype(&EVP_MD_CTX_free)>;

} // namespace

Result<std::array<std::uint8_t, 32>> artifact_digest(const Path& path) {
    std::ifstream file(path, std::ios::binary);
    if (!file) return std::unexpected(Error::io(path, std::error_code(errno ? errno : ENOENT, std::generic_category())));
    MdCtx ctx(EVP_MD_CTX_new(), &EVP_MD_CTX_free);
    EVP_DigestInit_ex(ctx.get(), EVP_sha256(), nullptr);
    std::vector<char> buf(64 * 1024);
    while (file) {
        file.read(buf.data(), static_cast<std::streamsize>(buf.size()));
        if (file.gcount() > 0) EVP_DigestUpdate(ctx.get(), buf.data(), static_cast<std::size_t>(file.gcount()));
    }
    if (file.bad()) return std::unexpected(Error::io(path, std::error_code(EIO, std::generic_category())));
    std::array<std::uint8_t, 32> out{};
    EVP_DigestFinal_ex(ctx.get(), out.data(), nullptr);
    return out;
}

bool verify_strict(const VerifyingKey& key, std::span<const std::uint8_t> message, std::span<const std::uint8_t, 64> sig) {
    // dalek's verify_strict refuses small-order keys and R; OpenSSL checks the rest (canonical S).
    if (has_small_order(key.bytes.data()) || has_small_order(sig.data())) return false;
    PKey pkey(EVP_PKEY_new_raw_public_key(EVP_PKEY_ED25519, nullptr, key.bytes.data(), key.bytes.size()), &EVP_PKEY_free);
    if (!pkey) return false;
    MdCtx ctx(EVP_MD_CTX_new(), &EVP_MD_CTX_free);
    if (!ctx || EVP_DigestVerifyInit(ctx.get(), nullptr, nullptr, nullptr, pkey.get()) != 1) return false;
    return EVP_DigestVerify(ctx.get(), sig.data(), sig.size(), message.data(), message.size()) == 1;
}

Result<> verify_staged(const Staged& staged, std::span<const VerifyingKey> trusted) {
    const auto artifact = staged.artifact();
    const auto signature = staged.signature();
    if (!signature) return std::unexpected(Error::unsigned_artifact(artifact));
    auto digest = artifact_digest(artifact);
    if (!digest) return std::unexpected(digest.error());
    const auto actual = to_hex(*digest);
    std::string expected = staged.manifest.sha256;
    std::ranges::transform(expected, expected.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    if (actual != expected) return std::unexpected(Error::mismatch("sha256", expected, actual));
    std::ifstream in(*signature, std::ios::binary);
    if (!in) {
        if (errno == ENOENT) return std::unexpected(Error::unsigned_artifact(artifact));
        return std::unexpected(Error::io(*signature, std::error_code(errno, std::generic_category())));
    }
    std::string text((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
    while (!text.empty() && (text.back() == '\n' || text.back() == '\r')) text.pop_back();
    const auto bytes = from_hex(text);
    if (!bytes) return std::unexpected(Error::signature("malformed signature: invalid hex"));
    if (bytes->size() != 64)
        return std::unexpected(Error::signature("signature has " + std::to_string(bytes->size()) + " bytes, expected 64"));
    const std::span<const std::uint8_t, 64> sig(bytes->data(), 64);
    for (const auto& key : trusted)
        if (verify_strict(key, *digest, sig)) return {};
    return std::unexpected(Error::signature(artifact.string() + " is not signed by a trusted key"));
}

Result<VerifyingKey> parse_public_key(std::string_view key_hex) {
    const auto ws = [](char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r'; };
    while (!key_hex.empty() && ws(key_hex.front())) key_hex.remove_prefix(1);
    while (!key_hex.empty() && ws(key_hex.back())) key_hex.remove_suffix(1);
    const auto bytes = from_hex(key_hex);
    if (!bytes) return std::unexpected(Error::signature("invalid public key: invalid hex"));
    if (bytes->size() != 32) return std::unexpected(Error::signature("invalid public key: " + std::to_string(bytes->size()) + " bytes, expected 32"));
    VerifyingKey key;
    std::ranges::copy(*bytes, key.bytes.begin());
    return key;
}

} // namespace signature
} // namespace loa::updater
