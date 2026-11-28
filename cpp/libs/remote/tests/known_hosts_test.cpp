// `known_hosts` statuses for hashed, ported, negated, revoked and certified entries.

#include <doctest/doctest.h>

#include <openssl/evp.h>
#include <openssl/hmac.h>

#include <filesystem>
#include <fstream>

#include "loa/remote/known_hosts.hpp"
#include "loa/runtime/tempfile.hpp"
#include "sshd.hpp"

using namespace loa::remote;
using namespace loa::remote::test;
namespace fs = std::filesystem;

namespace {

std::string b64(std::string_view bytes) {
    std::string out(4 * ((bytes.size() + 2) / 3) + 1, '\0');
    const int n = EVP_EncodeBlock(reinterpret_cast<unsigned char*>(out.data()), reinterpret_cast<const unsigned char*>(bytes.data()),
                                  static_cast<int>(bytes.size()));
    out.resize(static_cast<std::size_t>(n));
    return out;
}

std::string quoted(const fs::path& p) { return "'" + p.string() + "'"; }

// A host certificate for the key at `key` (its `.pub`), signed by `signer`, for `principal`.
PublicKey certificate(const fs::path& dir, const fs::path& key, const fs::path& signer, const std::string& principal, const std::string& name) {
    const auto copy = dir / (name + ".pub");
    fs::copy_file(fs::path(key.string() + ".pub"), copy, fs::copy_options::overwrite_existing);
    REQUIRE(sh("ssh-keygen -q -s " + quoted(signer) + " -I test -h -n " + principal + " -V -1m:+1h " + quoted(copy) + " 2>/dev/null") == 0);
    return public_key(dir / (name + "-cert.pub"));
}

} // namespace

TEST_CASE("statuses for hashed, ported, negated, revoked and certified entries") {
    const auto dir = *loa::runtime::TempDir::create();
    const auto& d = dir.path();
    for (const auto* name : {"a", "b", "old", "bad", "ca", "rogue"}) keygen(d / name);
    const auto key = [&](const char* name) { return public_key(d / (std::string(name) + ".pub")); };
    const auto line = [&](const char* name) { return key(name).to_openssh(); };

    const std::string salt = "0123456789abcdefghij";
    unsigned char mac[EVP_MAX_MD_SIZE];
    unsigned int mac_len = 0;
    HMAC(EVP_sha1(), salt.data(), static_cast<int>(salt.size()), reinterpret_cast<const unsigned char*>("web.example"), 11, mac, &mac_len);
    const auto hashed = "|1|" + b64(salt) + "|" + b64(std::string_view(reinterpret_cast<const char*>(mac), mac_len));
    const auto file = d / "sub/kh";
    const auto global = d / "global";
    const auto text = hashed + " " + line("a") + "\n[box.lan]:2222 " + line("b") + "\n*.lan,!secret.lan " + line("old") +
                      "\n@cert-authority *.ca " + line("ca") + "\n";
    std::ofstream(global) << "@revoked * " << line("bad") << "\n";
    const KnownHosts kh({file, global}, file);
    CHECK(kh.check("10.0.0.5", 2200, key("a"))->kind == HostKeyStatus::Kind::unknown);
    REQUIRE(kh.add("10.0.0.5", 2200, key("a")));
    CHECK(slurp(file).starts_with("[10.0.0.5]:2200 ssh-ed25519 "));
    std::ofstream(file, std::ios::trunc) << text << "# no newline";
    REQUIRE(kh.add("h", 22, key("a")));
    CHECK(slurp(file).ends_with("# no newline\nh " + line("a") + "\n"));

    const HostKeyStatus trusted{HostKeyStatus::Kind::trusted, {}, {}};
    const HostKeyStatus unknown{HostKeyStatus::Kind::unknown, {}, {}};
    const HostKeyStatus changed{HostKeyStatus::Kind::changed, {}, {KnownKey{file, 3, "ssh-ed25519", fingerprint(key("old"))}}};
    const std::vector<std::tuple<std::string, std::uint16_t, PublicKey, HostKeyStatus>> cases{
        {"web.example", 22, key("a"), trusted},
        {"other.example", 22, key("a"), unknown},
        {"h", 22, key("a"), trusted},
        {"box.lan", 2222, key("b"), trusted},
        {"box.lan", 22, key("b"), changed},
        {"db.lan", 22, key("b"), changed},
        {"secret.lan", 22, key("b"), unknown},
        {"db.ca", 22, certificate(d, d / "b", d / "ca", "db.ca", "c1"), trusted},
        {"db.ca", 22, certificate(d, d / "b", d / "ca", "web.ca", "c2"), unknown},
        {"db.ca", 22, certificate(d, d / "b", d / "rogue", "db.ca", "c3"), unknown},
    };
    for (const auto& [host, port, presented, want] : cases) {
        CAPTURE(host);
        CAPTURE(port);
        const auto got = kh.check(host, port, presented);
        REQUIRE(got);
        CHECK(*got == want);
    }
    const auto revoked = kh.check("db.lan", 22, key("bad"));
    REQUIRE(revoked);
    CHECK(revoked->kind == HostKeyStatus::Kind::revoked);
    CHECK(revoked->entry->file == global);
    CHECK(revoked->entry->line == 1);
}

TEST_CASE("keys parse, print and fingerprint like OpenSSH") {
    const auto dir = *loa::runtime::TempDir::create();
    keygen(dir.path() / "k");
    const auto key = public_key(dir.path() / "k.pub");
    CHECK(key.type == "ssh-ed25519");
    CHECK_FALSE(key.is_certificate());
    CHECK(slurp(dir.path() / "k.pub").starts_with(key.to_openssh() + " "));
    REQUIRE(sh("ssh-keygen -l -E sha256 -f '" + (dir.path() / "k.pub").string() + "' > '" + (dir.path() / "fp").string() + "'") == 0);
    CHECK(slurp(dir.path() / "fp").find(fingerprint(key)) != std::string::npos);
    const auto cert = certificate(dir.path(), dir.path() / "k", dir.path() / "k", "x", "c");
    CHECK(cert.is_certificate());
    CHECK(cert.algorithm() == "ssh-ed25519");
    CHECK(cert.key_data() == key);
    CHECK(fingerprint(cert) == fingerprint(key));
    CHECK(host_name("h", 22) == "h");
    CHECK(host_name("::1", 2200) == "[::1]:2200");
    CHECK_FALSE(PublicKey::from_openssh("ssh-rsa " + key.to_openssh().substr(12)));
}
