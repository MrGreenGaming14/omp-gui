#include "loa/remote/known_hosts.hpp"

#include <openssl/bn.h>
#include <openssl/core_names.h>
#include <openssl/ecdsa.h>
#include <openssl/evp.h>
#include <openssl/hmac.h>
#include <openssl/param_build.h>
#include <openssl/sha.h>

#include <algorithm>
#include <cctype>
#include <fstream>
#include <memory>
#include <sstream>
#include <system_error>

#include "loa/runtime/ssh.hpp"
#include "loa/runtime/time.hpp"
#include "wire.hpp"

namespace loa::remote {

namespace {

constexpr std::string_view CERT_SUFFIX = "-cert-v01@openssh.com";

// The number of wire fields after the type (and, for certificates, the nonce) of a plain key.
std::optional<int> key_fields(std::string_view type) {
    if (type == "ssh-ed25519") return 1;
    if (type == "ssh-rsa" || type == "sk-ssh-ed25519@openssh.com" || type.starts_with("ecdsa-sha2-")) return 2;
    if (type == "sk-ecdsa-sha2-nistp256@openssh.com") return 3;
    if (type == "ssh-dss") return 4;
    return std::nullopt;
}

// A parsed certificate.
struct Cert {
    std::string base;
    std::string key_blob;
    std::uint32_t cert_type = 0;
    std::vector<std::string> principals;
    std::uint64_t valid_after = 0;
    std::uint64_t valid_before = 0;
    std::string signature_key;
    std::string signed_data;
    std::string signature;
};

std::optional<Cert> parse_cert(std::string_view blob) {
    wire::Reader r(blob);
    const auto type = r.string();
    if (!type || !type->ends_with(CERT_SUFFIX)) return std::nullopt;
    Cert c;
    c.base = std::string(type->substr(0, type->size() - CERT_SUFFIX.size()));
    const auto fields = key_fields(c.base);
    if (!fields || !r.string()) return std::nullopt; // nonce
    wire::put_string(c.key_blob, c.base);
    const auto start = r.offset();
    for (int i = 0; i < *fields; ++i) {
        if (!r.string()) return std::nullopt;
    }
    c.key_blob.append(blob.substr(start, r.offset() - start));
    const auto serial = r.u64();
    const auto cert_type = r.u32();
    const auto key_id = r.string();
    const auto principals = r.string();
    const auto after = r.u64();
    const auto before = r.u64();
    const auto critical = r.string();
    const auto extensions = r.string();
    const auto reserved = r.string();
    const auto sig_key = r.string();
    if (!serial || !cert_type || !key_id || !principals || !after || !before || !critical || !extensions || !reserved || !sig_key) {
        return std::nullopt;
    }
    c.signed_data = std::string(blob.substr(0, r.offset()));
    const auto signature = r.string();
    if (!signature || !r.empty()) return std::nullopt;
    c.cert_type = *cert_type;
    wire::Reader p(*principals);
    while (!p.empty()) {
        const auto s = p.string();
        if (!s) return std::nullopt;
        c.principals.emplace_back(*s);
    }
    c.valid_after = *after;
    c.valid_before = *before;
    c.signature_key = std::string(*sig_key);
    c.signature = std::string(*signature);
    return c;
}

template <typename T, void (*F)(T*)>
struct Free {
    void operator()(T* p) const { F(p); }
};
using Pkey = std::unique_ptr<EVP_PKEY, Free<EVP_PKEY, EVP_PKEY_free>>;
using MdCtx = std::unique_ptr<EVP_MD_CTX, Free<EVP_MD_CTX, EVP_MD_CTX_free>>;
using Bn = std::unique_ptr<BIGNUM, Free<BIGNUM, BN_free>>;

Bn mpint(std::string_view bytes) {
    return Bn(BN_bin2bn(reinterpret_cast<const unsigned char*>(bytes.data()), static_cast<int>(bytes.size()), nullptr));
}

Pkey pkey_from_params(const char* type, OSSL_PARAM_BLD* bld) {
    OSSL_PARAM* params = OSSL_PARAM_BLD_to_param(bld);
    EVP_PKEY* key = nullptr;
    if (EVP_PKEY_CTX* ctx = EVP_PKEY_CTX_new_from_name(nullptr, type, nullptr)) {
        if (params && EVP_PKEY_fromdata_init(ctx) == 1) EVP_PKEY_fromdata(ctx, &key, EVP_PKEY_PUBLIC_KEY, params);
        EVP_PKEY_CTX_free(ctx);
    }
    OSSL_PARAM_free(params);
    return Pkey(key);
}

bool digest_verify(EVP_PKEY* key, const EVP_MD* md, std::string_view data, std::string_view sig) {
    MdCtx ctx(EVP_MD_CTX_new());
    if (!ctx || EVP_DigestVerifyInit(ctx.get(), nullptr, md, nullptr, key) != 1) return false;
    return EVP_DigestVerify(ctx.get(), reinterpret_cast<const unsigned char*>(sig.data()), sig.size(),
                            reinterpret_cast<const unsigned char*>(data.data()), data.size()) == 1;
}

// Verifies an SSH signature blob (`string alg, string sig`) over `data` with the key `key_blob`.
bool verify(std::string_view key_blob, std::string_view data, std::string_view signature) {
    wire::Reader s(signature);
    const auto alg = s.string();
    const auto sig = s.string();
    wire::Reader k(key_blob);
    const auto key_type = k.string();
    if (!alg || !sig || !key_type) return false;
    if (*key_type == "ssh-ed25519") {
        const auto pk = k.string();
        if (*alg != "ssh-ed25519" || !pk || pk->size() != 32) return false;
        Pkey key(EVP_PKEY_new_raw_public_key(EVP_PKEY_ED25519, nullptr, reinterpret_cast<const unsigned char*>(pk->data()), 32));
        return key && digest_verify(key.get(), nullptr, data, *sig);
    }
    if (*key_type == "ssh-rsa") {
        const auto e = k.string();
        const auto n = k.string();
        const EVP_MD* md = *alg == "rsa-sha2-256" ? EVP_sha256() : *alg == "rsa-sha2-512" ? EVP_sha512() : *alg == "ssh-rsa" ? EVP_sha1() : nullptr;
        if (!e || !n || !md) return false;
        const auto bn_e = mpint(*e);
        const auto bn_n = mpint(*n);
        OSSL_PARAM_BLD* bld = OSSL_PARAM_BLD_new();
        OSSL_PARAM_BLD_push_BN(bld, OSSL_PKEY_PARAM_RSA_N, bn_n.get());
        OSSL_PARAM_BLD_push_BN(bld, OSSL_PKEY_PARAM_RSA_E, bn_e.get());
        const auto key = pkey_from_params("RSA", bld);
        OSSL_PARAM_BLD_free(bld);
        return key && digest_verify(key.get(), md, data, *sig);
    }
    if (key_type->starts_with("ecdsa-sha2-") && *alg == *key_type) {
        const auto curve = k.string();
        const auto q = k.string();
        if (!curve || !q) return false;
        const char* group = *curve == "nistp256" ? "prime256v1" : *curve == "nistp384" ? "secp384r1" : *curve == "nistp521" ? "secp521r1" : nullptr;
        const EVP_MD* md = *curve == "nistp256" ? EVP_sha256() : *curve == "nistp384" ? EVP_sha384() : EVP_sha512();
        if (!group) return false;
        wire::Reader rs(*sig);
        const auto r = rs.string();
        const auto sv = rs.string();
        if (!r || !sv) return false;
        ECDSA_SIG* es = ECDSA_SIG_new();
        ECDSA_SIG_set0(es, mpint(*r).release(), mpint(*sv).release());
        unsigned char* der = nullptr;
        const int der_len = i2d_ECDSA_SIG(es, &der);
        ECDSA_SIG_free(es);
        if (der_len <= 0) return false;
        const std::string der_sig(reinterpret_cast<const char*>(der), static_cast<std::size_t>(der_len));
        OPENSSL_free(der);
        OSSL_PARAM_BLD* bld = OSSL_PARAM_BLD_new();
        OSSL_PARAM_BLD_push_utf8_string(bld, OSSL_PKEY_PARAM_GROUP_NAME, group, 0);
        OSSL_PARAM_BLD_push_octet_string(bld, OSSL_PKEY_PARAM_PUB_KEY, q->data(), q->size());
        const auto key = pkey_from_params("EC", bld);
        OSSL_PARAM_BLD_free(bld);
        return key && digest_verify(key.get(), md, data, der_sig);
    }
    return false;
}

std::string lower(std::string_view s) {
    std::string out(s);
    for (auto& c : out) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
    return out;
}

bool is_space(char c) { return std::isspace(static_cast<unsigned char>(c)) != 0; }

std::string_view trim(std::string_view s) {
    while (!s.empty() && is_space(s.front())) s.remove_prefix(1);
    while (!s.empty() && is_space(s.back())) s.remove_suffix(1);
    return s;
}

std::string_view trim_start(std::string_view s) {
    while (!s.empty() && is_space(s.front())) s.remove_prefix(1);
    return s;
}

// Splits at the first whitespace character (dropping it), as `split_once(char::is_whitespace)`.
std::optional<std::pair<std::string_view, std::string_view>> split_ws(std::string_view s) {
    const auto it = std::ranges::find_if(s, is_space);
    if (it == s.end()) return std::nullopt;
    const auto i = static_cast<std::size_t>(it - s.begin());
    return std::pair{s.substr(0, i), s.substr(i + 1)};
}

enum class Marker { none, cert_authority, revoked };

struct Entry {
    Marker marker = Marker::none;
    std::string hosts;
    PublicKey key;
    std::filesystem::path file;
    std::size_t line = 0;

    [[nodiscard]] KnownKey known() const { return KnownKey{file, line, key.algorithm(), fingerprint(key)}; }

    [[nodiscard]] bool matches_host(const std::string& name) const {
        if (std::string_view(hosts).starts_with("|1|")) return hashed_matches(std::string_view(hosts).substr(3), name);
        bool matched = false;
        std::size_t start = 0;
        for (;;) {
            const auto end = std::min(hosts.find(',', start), hosts.size());
            std::string_view pattern = std::string_view(hosts).substr(start, end - start);
            const bool negated = pattern.starts_with('!');
            if (negated) pattern.remove_prefix(1);
            if (loa::runtime::ssh::wildcard_match(lower(pattern), name)) {
                if (negated) return false;
                matched = true;
            }
            if (end >= hosts.size()) break;
            start = end + 1;
        }
        return matched;
    }

    static bool hashed_matches(std::string_view hashed, const std::string& name) {
        const auto bar = hashed.find('|');
        if (bar == std::string_view::npos) return false;
        const auto salt = wire::base64_decode(hashed.substr(0, bar));
        const auto hash = wire::base64_decode(hashed.substr(bar + 1));
        if (!salt || !hash) return false;
        unsigned char mac[EVP_MAX_MD_SIZE];
        unsigned int len = 0;
        if (!HMAC(EVP_sha1(), salt->data(), static_cast<int>(salt->size()), reinterpret_cast<const unsigned char*>(name.data()),
                  name.size(), mac, &len)) {
            return false;
        }
        return hash->size() == len && CRYPTO_memcmp(hash->data(), mac, len) == 0;
    }
};

// Parses `known_hosts` text; malformed lines are skipped, as OpenSSH does.
void parse_into(std::string_view text, const std::filesystem::path& file, std::vector<Entry>& out) {
    std::size_t index = 0;
    std::size_t start = 0;
    while (start < text.size()) {
        auto end = text.find('\n', start);
        if (end == std::string_view::npos) end = text.size();
        auto raw = text.substr(start, end - start);
        if (raw.ends_with('\r')) raw.remove_suffix(1);
        start = end + 1;
        ++index;
        const auto line = trim(raw);
        if (line.empty() || line.starts_with('#')) continue;
        Marker marker = Marker::none;
        std::string_view rest = line;
        if (line.starts_with('@')) {
            const auto body = line.substr(1);
            const auto parts = split_ws(body);
            const auto word = parts ? parts->first : body;
            const auto after = parts ? parts->second : std::string_view();
            if (word == "cert-authority") marker = Marker::cert_authority;
            else if (word == "revoked") marker = Marker::revoked;
            else continue;
            rest = trim_start(after);
        }
        const auto hosts_key = split_ws(rest);
        if (!hosts_key) continue;
        auto key = PublicKey::from_openssh(trim(hosts_key->second));
        if (!key) continue;
        out.push_back(Entry{marker, std::string(hosts_key->first), std::move(*key), file, index});
    }
}

std::optional<HostKeyStatus> revoked(const std::vector<const Entry*>& entries, const PublicKey& key) {
    for (const auto* e : entries) {
        if (e->marker == Marker::revoked && e->key.key_data() == key) return HostKeyStatus{HostKeyStatus::Kind::revoked, e->known(), {}};
    }
    return std::nullopt;
}

HostKeyStatus check_key(const std::vector<const Entry*>& entries, const PublicKey& key) {
    if (auto status = revoked(entries, key)) return std::move(*status);
    std::vector<const Entry*> plain;
    for (const auto* e : entries) {
        if (e->marker == Marker::none) plain.push_back(e);
    }
    if (std::ranges::any_of(plain, [&](const Entry* e) { return e->key.key_data() == key; })) return {HostKeyStatus::Kind::trusted, {}, {}};
    if (plain.empty()) return {HostKeyStatus::Kind::unknown, {}, {}};
    HostKeyStatus status{HostKeyStatus::Kind::changed, {}, {}};
    for (const auto* e : plain) status.previous.push_back(e->known());
    return status;
}

HostKeyStatus check_certificate(const std::vector<const Entry*>& entries, const PublicKey& presented, std::string_view hostname) {
    const auto cert = parse_cert(presented.blob);
    if (!cert) return check_key(entries, presented.key_data());
    const auto ca = PublicKey::from_blob(cert->signature_key);
    if (!ca) return check_key(entries, presented.key_data());
    if (auto status = revoked(entries, *ca)) return std::move(*status);
    if (std::ranges::any_of(entries, [&](const Entry* e) { return e->marker == Marker::cert_authority && e->key.key_data() == *ca; })) {
        const bool principal_ok = cert->principals.empty() || std::ranges::find(cert->principals, hostname) != cert->principals.end();
        const auto now = loa::runtime::time::now_secs();
        const bool valid = verify(cert->signature_key, cert->signed_data, cert->signature) && cert->valid_after <= now && now < cert->valid_before;
        if (cert->cert_type == 2 && principal_ok && valid) return {HostKeyStatus::Kind::trusted, {}, {}};
    }
    // Not vouched for by a CA on file: judge the certified key itself.
    return check_key(entries, presented.key_data());
}

} // namespace

std::optional<PublicKey> PublicKey::from_blob(std::string blob) {
    wire::Reader r(blob);
    const auto type = r.string();
    if (!type || type->empty()) return std::nullopt;
    std::string t(*type);
    return PublicKey{std::move(t), std::move(blob)};
}

std::optional<PublicKey> PublicKey::from_openssh(std::string_view line) {
    line = trim(line);
    const auto parts = split_ws(line);
    const auto type = parts ? parts->first : line;
    auto data = parts ? trim_start(parts->second) : std::string_view();
    if (const auto more = split_ws(data)) data = more->first;
    if (type.empty() || data.empty()) return std::nullopt;
    auto blob = wire::base64_decode(data);
    if (!blob) return std::nullopt;
    auto key = from_blob(std::move(*blob));
    if (!key || key->type != type) return std::nullopt;
    return key;
}

bool PublicKey::is_certificate() const noexcept { return std::string_view(type).ends_with(CERT_SUFFIX); }

std::string PublicKey::to_openssh() const { return type + " " + wire::base64_encode(blob); }

std::string PublicKey::algorithm() const {
    if (is_certificate()) return type.substr(0, type.size() - CERT_SUFFIX.size());
    return type;
}

PublicKey PublicKey::key_data() const {
    if (!is_certificate()) return *this;
    if (const auto cert = parse_cert(blob)) return PublicKey{cert->base, cert->key_blob};
    return *this;
}

std::string host_name(std::string_view hostname, std::uint16_t port) {
    if (port == 22) return std::string(hostname);
    return "[" + std::string(hostname) + "]:" + std::to_string(port);
}

std::string fingerprint(const PublicKey& key) {
    const auto plain = key.key_data();
    unsigned char digest[SHA256_DIGEST_LENGTH];
    SHA256(reinterpret_cast<const unsigned char*>(plain.blob.data()), plain.blob.size(), digest);
    auto b64 = wire::base64_encode(std::string_view(reinterpret_cast<const char*>(digest), sizeof digest));
    while (b64.ends_with('=')) b64.pop_back();
    return "SHA256:" + b64;
}

Result<HostKeyStatus> KnownHosts::check(std::string_view hostname, std::uint16_t port, const PublicKey& presented) const {
    const auto name = lower(host_name(hostname, port));
    std::vector<Entry> all;
    for (const auto& path : files_) {
        std::ifstream in(path, std::ios::binary);
        if (!in) {
            std::error_code ec;
            if (!std::filesystem::exists(path, ec) && !ec) continue;
            return std::unexpected(Error::io(path.string(), std::error_code(errno, std::generic_category()).message()));
        }
        std::ostringstream text;
        text << in.rdbuf();
        parse_into(text.str(), path, all);
    }
    std::vector<const Entry*> entries;
    for (const auto& e : all) {
        if (e.matches_host(name)) entries.push_back(&e);
    }
    if (presented.is_certificate()) return check_certificate(entries, presented, hostname);
    return check_key(entries, presented);
}

Result<void> KnownHosts::add(std::string_view hostname, std::uint16_t port, const PublicKey& key) const {
    const auto fail = [&](std::string message) { return std::unexpected(Error::known_hosts(write_to_.string(), std::move(message))); };
    const auto line = key.key_data().to_openssh();
    std::lock_guard guard(write_lock_);
    std::error_code ec;
    if (write_to_.has_parent_path()) {
        std::filesystem::create_directories(write_to_.parent_path(), ec);
        if (ec) return fail(ec.message());
    }
    bool needs_newline = false;
    if (std::ifstream in(write_to_, std::ios::binary | std::ios::ate); in) {
        const auto size = static_cast<std::streamoff>(in.tellg());
        if (size > 0) {
            in.seekg(size - 1);
            needs_newline = in.get() != '\n';
        }
    } else if (std::filesystem::exists(write_to_, ec)) {
        return fail("cannot read the file");
    }
    const auto text = std::string(needs_newline ? "\n" : "") + host_name(hostname, port) + " " + line + "\n";
    std::ofstream out(write_to_, std::ios::binary | std::ios::app);
    if (!out || !(out << text) || !out.flush()) return fail(std::error_code(errno, std::generic_category()).message());
    return {};
}

} // namespace loa::remote
