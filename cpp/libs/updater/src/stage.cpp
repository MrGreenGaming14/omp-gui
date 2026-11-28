#include "loa/updater/stage.hpp"

#include <openssl/evp.h>
#include <sys/stat.h>
#include <unistd.h>

#include <algorithm>
#include <cerrno>
#include <filesystem>
#include <fstream>
#include <memory>

#include "loa/runtime/fs.hpp"
#include "loa/updater/signature.hpp"
#include "loa/updater/unpack.hpp"
#include "util.hpp"

namespace loa::updater {

using namespace detail;
namespace stdfs = std::filesystem;

namespace {

constexpr std::uint64_t SIGNATURE_LIMIT = 64 * 1024;
constexpr std::uint64_t MAX_ARTIFACT_SIZE = 1024ull * 1024 * 1024;

Result<> single_name(std::string_view name) {
    if (name.empty() || name == "." || name == ".." || name.find('/') != std::string_view::npos || name.find('\0') != std::string_view::npos)
        return std::unexpected(Error::bad_name(name));
    return {};
}

std::error_code last_error() { return {errno, std::generic_category()}; }

Result<std::optional<Staged>> read_staged(const Path& dir) {
    auto r = read_json<StagedManifest>(dir / std::string(MANIFEST_FILE), staged_manifest_from_json);
    if (!r) return std::unexpected(r.error());
    if (!*r) return std::optional<Staged>{};
    return std::optional<Staged>{Staged{dir, std::move(**r)}};
}

// A temp directory created with `.staging-` in the product directory; removed unless kept.
class StagingDir {
  public:
    static Result<StagingDir> create(const Path& parent) {
        std::string name = (parent / ".staging-XXXXXX").string();
        std::vector<char> buf(name.begin(), name.end());
        buf.push_back('\0');
        char* path = ::mkdtemp(buf.data());
        if (!path) return std::unexpected(Error::io(parent, last_error()));
        return StagingDir(path);
    }
    StagingDir(const StagingDir&) = delete;
    StagingDir& operator=(const StagingDir&) = delete;
    StagingDir(StagingDir&& other) noexcept : path_(std::move(other.path_)), kept_(other.kept_) { other.kept_ = true; }
    ~StagingDir() {
        if (!kept_) {
            std::error_code ec;
            stdfs::remove_all(path_, ec);
        }
    }
    [[nodiscard]] const Path& path() const { return path_; }
    void keep() noexcept { kept_ = true; }

  private:
    explicit StagingDir(Path path) : path_(std::move(path)) {}
    Path path_;
    bool kept_ = false;
};

struct Downloaded {
    std::uint64_t size = 0;
    std::string sha256;
    std::optional<std::string> sha512;
};

using MdCtx = std::unique_ptr<EVP_MD_CTX, decltype(&EVP_MD_CTX_free)>;

// Streams `url` into `dest`, hashing as it writes; fails once more than `limit` bytes arrive.
Result<Downloaded> download(const Http& client, const std::string& url, const Path& dest, std::uint64_t limit, bool sha512) {
    FILE* raw = std::fopen(dest.c_str(), "wb");
    if (!raw) return std::unexpected(Error::io(dest, last_error()));
    std::unique_ptr<FILE, decltype(&std::fclose)> file(raw, &std::fclose);
    MdCtx h256(EVP_MD_CTX_new(), &EVP_MD_CTX_free);
    MdCtx h512(sha512 ? EVP_MD_CTX_new() : nullptr, &EVP_MD_CTX_free);
    if (!h256 || (sha512 && !h512)) return std::unexpected(Error::archive("cannot create hash context"));
    EVP_DigestInit_ex(h256.get(), EVP_sha256(), nullptr);
    if (h512) EVP_DigestInit_ex(h512.get(), EVP_sha512(), nullptr);
    Downloaded got;
    const Http::OnHead on_head = [&](long status, const HttpHeaders&, std::optional<std::uint64_t> length) -> std::optional<Error> {
        if (status < 200 || status >= 300) return Error::status(url, status);
        if (length && *length > limit) return Error::too_large(url, limit);
        return std::nullopt;
    };
    const Http::OnChunk on_chunk = [&](std::span<const char> chunk) -> std::optional<Error> {
        if (chunk.size() > limit - got.size) return Error::too_large(url, limit);
        got.size += chunk.size();
        EVP_DigestUpdate(h256.get(), chunk.data(), chunk.size());
        if (h512) EVP_DigestUpdate(h512.get(), chunk.data(), chunk.size());
        if (std::fwrite(chunk.data(), 1, chunk.size(), file.get()) != chunk.size()) return Error::io(dest, last_error());
        return std::nullopt;
    };
    if (auto r = client.stream(url, {}, on_head, on_chunk); !r) return std::unexpected(r.error());
    if (std::fflush(file.get()) != 0 || ::fsync(::fileno(file.get())) != 0) return std::unexpected(Error::io(dest, last_error()));
    file.reset();
    std::array<std::uint8_t, 32> d256{};
    EVP_DigestFinal_ex(h256.get(), d256.data(), nullptr);
    got.sha256 = to_hex(d256);
    if (h512) {
        std::array<std::uint8_t, 64> d512{};
        EVP_DigestFinal_ex(h512.get(), d512.data(), nullptr);
        got.sha512 = to_hex(d512);
    }
    return got;
}

Result<> verify_download(const loa::settings::Artifact& artifact, const loa::settings::Checksum& checksum, const Downloaded& got) {
    if (artifact.size && *artifact.size != got.size) return std::unexpected(Error::mismatch("size", std::to_string(*artifact.size), std::to_string(got.size)));
    const bool sha512 = checksum.algorithm == loa::settings::Checksum::Algorithm::Sha512;
    const auto& actual = sha512 ? got.sha512.value_or("") : got.sha256;
    const auto same = checksum.hex.size() == actual.size() && std::ranges::equal(checksum.hex, actual, [](unsigned char a, unsigned char b) {
        return std::tolower(a) == std::tolower(b);
    });
    if (!same) return std::unexpected(Error::mismatch(sha512 ? "sha512" : "sha256", checksum.hex, actual));
    return {};
}

} // namespace

namespace stage {

Path product_dir(const Path& root, std::string_view product) { return root / "versions" / std::string(product); }
Path version_dir(const Path& root, std::string_view product, const Version& version) { return product_dir(root, product) / version.to_string(); }
Result<std::optional<Staged>> staged(const Path& root, std::string_view product, const Version& version) {
    return read_staged(version_dir(root, product, version));
}

} // namespace stage

std::optional<Path> Staged::signature() const {
    if (!manifest.signature_name) return std::nullopt;
    return dir / "artifact" / *manifest.signature_name;
}

Json to_json(const StagedManifest& v) {
    return Object{{"product", v.product},
                  {"version", v.version.to_string()},
                  {"artifact_name", v.artifact_name},
                  {"sha256", v.sha256},
                  {"size", v.size},
                  {"signature_name", opt(v.signature_name)}};
}

Decoded<StagedManifest> staged_manifest_from_json(const Json& v) {
    return decoding([&] {
        const auto& o = object(v, "struct StagedManifest");
        return StagedManifest{string(required(o, "product")), version(required(o, "version")), string(required(o, "artifact_name")),
                              string(required(o, "sha256")), u64(required(o, "size")), opt_string(field(o, "signature_name"))};
    });
}

Result<Staged> Stager::stage(const loa::settings::Release& release) const {
    const auto unverifiable = [&](std::string_view what) { return Error::unverifiable(release.product, release.version, what); };
    if (!release.artifact) return std::unexpected(unverifiable("artifact for this platform"));
    const auto& artifact = *release.artifact;
    if (!artifact.checksum) return std::unexpected(unverifiable("checksum"));
    if (auto r = single_name(release.product); !r) return std::unexpected(r.error());
    if (auto r = single_name(artifact.name); !r) return std::unexpected(r.error());
    if (artifact.size && *artifact.size > MAX_ARTIFACT_SIZE) return std::unexpected(Error::too_large(artifact.url, MAX_ARTIFACT_SIZE));
    const auto final_dir = stage::version_dir(root_, release.product, release.version);
    auto already = read_staged(final_dir);
    if (!already) return std::unexpected(already.error());
    if (*already) return std::move(**already);
    std::error_code ec;
    if (stdfs::exists(final_dir, ec)) return std::unexpected(Error::unsafe_entry(final_dir.string(), "version directory exists without a staged manifest"));
    if (ec) return std::unexpected(Error::io(final_dir, ec));
    const auto product_dir = stage::product_dir(root_, release.product);
    stdfs::create_directories(product_dir, ec);
    if (ec) return std::unexpected(Error::io(product_dir, ec));
    auto tmp = StagingDir::create(product_dir);
    if (!tmp) return std::unexpected(tmp.error());
    const auto artifact_dir = tmp->path() / "artifact";
    if (::mkdir(artifact_dir.c_str(), 0777) != 0) return std::unexpected(Error::io(artifact_dir, last_error()));
    const auto artifact_path = artifact_dir / artifact.name;
    const bool sha512 = artifact.checksum->algorithm == loa::settings::Checksum::Algorithm::Sha512;
    auto got = download(client_, artifact.url, artifact_path, artifact.size.value_or(MAX_ARTIFACT_SIZE), sha512);
    if (!got) return std::unexpected(got.error());
    if (auto r = verify_download(artifact, *artifact.checksum, *got); !r) return std::unexpected(r.error());
    std::optional<std::string> signature_name;
    if (artifact.signature_url) {
        signature_name = artifact.name + ".sig";
        if (auto r = download(client_, *artifact.signature_url, artifact_dir / *signature_name, SIGNATURE_LIMIT, false); !r)
            return std::unexpected(r.error());
    }
    const auto payload = tmp->path() / "payload";
    if (auto r = unpack::unpacker(artifact.format)->unpack(artifact_path, payload); !r) return std::unexpected(r.error());
    StagedManifest manifest{release.product, release.version, artifact.name, got->sha256, got->size, signature_name};
    if (auto r = write_json(tmp->path() / std::string(MANIFEST_FILE), to_json(manifest)); !r) return std::unexpected(r.error());
    stdfs::rename(tmp->path(), final_dir, ec);
    if (ec) {
        // Another stager may have finished the same version first.
        auto other = read_staged(final_dir);
        if (other && *other) return std::move(**other);
        return std::unexpected(Error::io(final_dir, ec));
    }
    tmp->keep();
    if (auto r = loa::runtime::fs::sync_dir(product_dir); !r) return std::unexpected(Error::io(product_dir, r.error()));
    return Staged{final_dir, std::move(manifest)};
}

} // namespace loa::updater
