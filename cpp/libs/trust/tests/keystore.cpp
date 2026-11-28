#include <doctest/doctest.h>

#include <sys/stat.h>

#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <memory>
#include <string>
#include <vector>

#include "loa/runtime/tempfile.hpp"
#include "loa/trust/key.hpp"
#include "loa/trust/keystore.hpp"

#if defined(_WIN32)
#include <array>
#include <cstdio>
#endif

using namespace loa::trust;
namespace fs = std::filesystem;

namespace {

std::string slurp(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), {}};
}

std::vector<std::uint8_t> bytes(const std::string& s) { return {s.begin(), s.end()}; }

#if !defined(_WIN32)
unsigned mode_of(const fs::path& path) {
    struct stat st {};
    REQUIRE(::stat(path.c_str(), &st) == 0);
    return st.st_mode & 0777;
}
#endif

// The OS keychain when no Secret Service is reachable.
struct NoKeychain final : KeyStore {
    Result<std::optional<SecretBytes>> load() const override { return std::unexpected(TrustError::unavailable("no secret service")); }
    Result<void> store(std::span<const std::uint8_t>) const override { return std::unexpected(TrustError::unavailable("no secret service")); }
    Result<void> remove() const override { return {}; }
};

} // namespace

TEST_CASE("keystore_fallback_migrates_and_never_switches_on_bad_values") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const fs::path dir = tmp->path() / "loa-trust-keys";
    auto file = [&] { return std::make_shared<FileKeyStore>(FileKeyStore::in_dir(dir)); };

    const FallbackStore fallback(std::make_shared<NoKeychain>(), file());
    const auto first = NodeIdentity::load_or_create(fallback);
    REQUIRE(first);
    const auto source = fallback.source();
    const auto* in_file = std::get_if<key_source::File>(&source);
    REQUIRE(in_file);
    CHECK(in_file->keychain_error.find("secret service") != std::string::npos);
    CHECK(in_file->path.filename() == "node.key");
#if !defined(_WIN32)
    CHECK(mode_of(dir / "node.key") == 0600);
#endif

    // Once a keychain works, the file key moves into it and the address stays.
    const FallbackStore migrated(std::make_shared<MemoryKeyStore>(), file());
    const auto moved = NodeIdentity::load_or_create(migrated);
    REQUIRE(moved);
    CHECK(moved->public_key() == first->public_key());
    CHECK(migrated.source() == KeySource(key_source::Keychain{}));
    CHECK(!file()->load()->has_value());

    // A malformed keychain value is an error, not a reason to switch stores.
    const std::vector<std::uint8_t> short_secret = {1, 2, 3};
    const FallbackStore bad(std::make_shared<MemoryKeyStore>(MemoryKeyStore::with_secret(short_secret)), file());
    const auto refused = NodeIdentity::load_or_create(bad);
    REQUIRE(!refused);
    CHECK(refused.error().kind() == TrustError::Kind::wrong_length);
    CHECK(refused.error().found() == 3);
    CHECK(!file()->load()->has_value());
}

TEST_CASE("a wrong-length key file fails load_or_create and is left unchanged") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto store = FileKeyStore::in_dir(tmp->path());
    const std::vector<std::uint8_t> bad(31, 9);
    REQUIRE(store.store(bad));
    const auto loaded = NodeIdentity::load_or_create(store);
    REQUIRE(!loaded);
    CHECK(loaded.error().kind() == TrustError::Kind::wrong_length);
    CHECK(loaded.error().found() == 31);
    CHECK(bytes(slurp(tmp->path() / "node.key")) == bad);
}

TEST_CASE("file key store: replace, delete twice, missing is none") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto store = FileKeyStore::in_dir(tmp->path() / "nested");
    CHECK(store.path() == tmp->path() / "nested" / "node.key");
    CHECK(!store.load()->has_value());
    const std::vector<std::uint8_t> a(32, 7), b(32, 8);
    REQUIRE(store.store(a));
    REQUIRE(store.store(b));
    const auto loaded = store.load();
    REQUIRE(loaded);
    REQUIRE(loaded->has_value());
    CHECK(std::vector<std::uint8_t>((*loaded)->bytes().begin(), (*loaded)->bytes().end()) == b);
    REQUIRE(store.remove());
    REQUIRE(store.remove());
    CHECK(!store.load()->has_value());
}

#if !defined(_WIN32)
TEST_CASE("the node key file is 0600 even over a wider existing file") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto path = tmp->path() / "node.key";
    std::ofstream(path) << "old";
    fs::permissions(path, fs::perms(0644));
    REQUIRE(mode_of(path) == 0644);
    const auto store = FileKeyStore::in_dir(tmp->path());
    REQUIRE(store.store(std::vector<std::uint8_t>(32, 1)));
    CHECK(mode_of(path) == 0600);
}
#endif

#if defined(_WIN32)
TEST_CASE("persisted_key_does_not_inherit_access") {
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto file = FileKeyStore::in_dir(tmp->path());
    REQUIRE(file.store(std::vector<std::uint8_t>(32, 7)));
    REQUIRE(file.store(std::vector<std::uint8_t>(32, 8)));
    const auto loaded = file.load();
    REQUIRE(loaded);
    REQUIRE(loaded->has_value());
    CHECK(std::vector<std::uint8_t>((*loaded)->bytes().begin(), (*loaded)->bytes().end()) == std::vector<std::uint8_t>(32, 8));

    auto run = [](const std::string& command) {
        std::string out;
        FILE* pipe = _popen(command.c_str(), "r");
        REQUIRE(pipe);
        std::array<char, 256> buf{};
        while (fgets(buf.data(), static_cast<int>(buf.size()), pipe)) out += buf.data();
        REQUIRE(_pclose(pipe) == 0);
        return out;
    };
    const std::string text = run("icacls.exe \"" + file.path()->string() + "\"");
    std::vector<std::string> lines;
    for (std::size_t start = 0; start < text.size();) {
        const auto end = text.find('\n', start);
        std::string line = text.substr(start, end == std::string::npos ? std::string::npos : end - start);
        if (!line.empty() && line.back() == '\r') line.pop_back();
        if (line.find_first_not_of(" \t") == std::string::npos) break;
        lines.push_back(line);
        if (end == std::string::npos) break;
        start = end + 1;
    }
    CHECK(lines.size() == 1);
    CHECK(text.find("(I)") == std::string::npos);
    // The one entry grants the current user full control, not just anyone.
    std::string account = run("whoami");
    while (!account.empty() && (account.back() == '\n' || account.back() == '\r' || account.back() == ' ')) account.pop_back();
    REQUIRE(!account.empty());
    auto lower = [](std::string s) {
        for (auto& c : s) c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
        return s;
    };
    CHECK(lower(lines.at(0)).find(lower(account) + ":(f)") != std::string::npos);
}
#endif

TEST_CASE("golden: the Rust node.key loads to node.pub and the C++ store writes the same bytes") {
    const fs::path golden = fs::path(LOA_GOLDEN_DIR) / "trust";
    const auto rust_key = slurp(golden / "node.key");
    const auto pub = slurp(golden / "node.pub");
    REQUIRE(rust_key.size() == 32);

    // Rust-written file read by C++.
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    fs::copy_file(golden / "node.key", tmp->path() / "node.key");
    const auto read = FileKeyStore::in_dir(tmp->path());
    const auto loaded = NodeIdentity::load_or_create(read);
    REQUIRE(loaded);
    CHECK(loaded->public_key().to_string() + "\n" == pub);
    CHECK(slurp(tmp->path() / "node.key") == rust_key);

    // Key text round-trips.
    const auto parsed = PublicKey::parse(pub.substr(0, 64));
    REQUIRE(parsed);
    CHECK(parsed->to_string() + "\n" == pub);

    // C++-written file for the seed [7; 32] is byte-identical to the Rust one.
    std::array<std::uint8_t, 32> seed{};
    seed.fill(7);
    const auto id = NodeIdentity::from_secret_bytes(seed);
    auto out = loa::runtime::TempDir::create();
    REQUIRE(out);
    REQUIRE(FileKeyStore::in_dir(out->path()).store(id.secret_bytes()));
    CHECK(slurp(out->path() / "node.key") == rust_key);
#if !defined(_WIN32)
    CHECK(mode_of(out->path() / "node.key") == 0600);
#endif
    CHECK(id.public_key().to_string() + "\n" == pub);
}

#if !defined(_WIN32) && !defined(__APPLE__)
TEST_CASE("load_app_identity falls back to node.key when no Secret Service is reachable") {
    // Only this test talks to the keychain; point it at a bus that does not exist.
    ::setenv("DBUS_SESSION_BUS_ADDRESS", "unix:path=/nonexistent/loa-trust-test-bus", 1);
    auto tmp = loa::runtime::TempDir::create();
    REQUIRE(tmp);
    const auto first = load_app_identity(tmp->path());
    REQUIRE(first);
    const auto* in_file = std::get_if<key_source::File>(&first->second);
    REQUIRE(in_file);
    CHECK(in_file->path == tmp->path() / "node.key");
    CHECK(!in_file->keychain_error.empty());
    CHECK(mode_of(tmp->path() / "node.key") == 0600);
    const auto again = load_app_identity(tmp->path());
    REQUIRE(again);
    CHECK(again->first.public_key() == first->first.public_key());
}
#endif
