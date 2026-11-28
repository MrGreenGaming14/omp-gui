// Re-hashes every file listed in MANIFEST.md and compares length and sha256.

#include <doctest/doctest.h>
#include <openssl/evp.h>

#include <cstdint>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <memory>
#include <sstream>
#include <string>
#include <vector>

namespace {

struct Entry {
    std::string path;
    std::uint64_t length = 0;
    std::string sha256;
};

std::string trim(const std::string& s) {
    const auto b = s.find_first_not_of(" \t`");
    if (b == std::string::npos) return {};
    const auto e = s.find_last_not_of(" \t`");
    return s.substr(b, e - b + 1);
}

// Rows look like `| path | length | sha256 | rust sha |`; the header and the
// separator row are skipped because their length cell is not a number.
std::vector<Entry> read_manifest(const std::filesystem::path& file) {
    std::ifstream in(file);
    REQUIRE(in);
    std::vector<Entry> out;
    for (std::string line; std::getline(in, line);) {
        if (!line.starts_with("|")) continue;
        std::vector<std::string> cells;
        std::stringstream row(line.substr(1));
        for (std::string cell; std::getline(row, cell, '|');) cells.push_back(trim(cell));
        if (cells.size() < 3 || cells[1].empty() || cells[1].find_first_not_of("0123456789") != std::string::npos)
            continue;
        out.push_back({cells[0], std::stoull(cells[1]), cells[2]});
    }
    return out;
}

std::string sha256_hex(const std::string& bytes) {
    std::unique_ptr<EVP_MD_CTX, decltype(&EVP_MD_CTX_free)> ctx(EVP_MD_CTX_new(), &EVP_MD_CTX_free);
    REQUIRE(ctx);
    unsigned char digest[EVP_MAX_MD_SIZE];
    unsigned int len = 0;
    REQUIRE(EVP_DigestInit_ex(ctx.get(), EVP_sha256(), nullptr) == 1);
    REQUIRE(EVP_DigestUpdate(ctx.get(), bytes.data(), bytes.size()) == 1);
    REQUIRE(EVP_DigestFinal_ex(ctx.get(), digest, &len) == 1);
    static constexpr char hex[] = "0123456789abcdef";
    std::string out;
    for (unsigned int i = 0; i < len; ++i) {
        out += hex[digest[i] >> 4];
        out += hex[digest[i] & 0xf];
    }
    return out;
}

}  // namespace

TEST_CASE("every MANIFEST entry matches its length and sha256") {
    const std::filesystem::path root = LOA_GOLDEN_DIR;
    const auto entries = read_manifest(root / "MANIFEST.md");
    REQUIRE(entries.size() >= 10);
    for (const auto& e : entries) {
        CAPTURE(e.path);
        std::ifstream in(root / e.path, std::ios::binary);
        REQUIRE(in);
        const std::string bytes{std::istreambuf_iterator<char>(in), {}};
        CHECK(bytes.size() == e.length);
        CHECK(sha256_hex(bytes) == e.sha256);
    }
}

namespace {

std::string slurp(const std::filesystem::path& file) {
    std::ifstream in(file, std::ios::binary);
    REQUIRE(in);
    return {std::istreambuf_iterator<char>(in), {}};
}

}  // namespace

TEST_CASE("every p2p frame is a big-endian length prefix and one non-empty message of at most 1 MiB") {
    const std::filesystem::path dir = std::filesystem::path(LOA_GOLDEN_DIR) / "p2p";
    std::size_t frames = 0;
    for (const auto& file : std::filesystem::directory_iterator(dir)) {
        if (file.path().extension() != ".bin") continue;
        CAPTURE(file.path().filename().string());
        const auto bytes = slurp(file.path());
        REQUIRE(bytes.size() > 4);
        const auto* p = reinterpret_cast<const unsigned char*>(bytes.data());
        const std::uint32_t len = (std::uint32_t{p[0]} << 24) | (std::uint32_t{p[1]} << 16) | (std::uint32_t{p[2]} << 8) | p[3];
        CHECK(len == bytes.size() - 4);
        CHECK(len <= 1024u * 1024u);
        ++frames;
    }
    for (const char* body : {"hello", "presence", "grant-delivery", "grant-renewal", "grant-renewal-result", "grant-revocation", "ack"})
        CHECK(std::filesystem::exists(dir / (std::string("control-") + body + ".bin")));
    for (const char* body : {"subscribe", "update", "drive", "challenge", "present", "list-sessions", "session-list", "seed",
                             "drive-result", "prompt", "ack", "answer"})
        CHECK(std::filesystem::exists(dir / (std::string("frame-") + body + ".bin")));
    CHECK(frames >= 19 + 5);
    CHECK(slurp(dir / "labels.txt") == "omp/control/1\nomp/session/1\nomp/folder/1\n");
}

TEST_CASE("node.key is the 32-byte seed and the token is the loa-trust fixture") {
    const std::filesystem::path root = LOA_GOLDEN_DIR;
    CHECK(slurp(root / "trust/node.key").size() == 32);
    CHECK(slurp(root / "trust/v1_reshared_token.bin") ==
          slurp(root / "../../../crates/loa-trust/tests/fixtures/v1_reshared_token.bin"));
}
