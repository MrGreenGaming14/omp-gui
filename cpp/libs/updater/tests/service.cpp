#include <doctest/doctest.h>

#include <archive.h>
#include <archive_entry.h>
#include <httplib.h>
#include <openssl/evp.h>
#include <openssl/sha.h>

#include <filesystem>
#include <fstream>
#include <map>
#include <thread>

#include "loa/runtime/exec.hpp"
#include "loa/runtime/tempfile.hpp"
#include "loa/updater/feeds.hpp"
#include "loa/updater/service.hpp"
#include "loa/updater/state.hpp"
#include "loa/updater/unpack.hpp"

using namespace loa::updater;
using loa::settings::ArchiveFormat;
using loa::settings::CheckResult;
using loa::settings::Checksum;
using loa::settings::GateCheck;
using loa::settings::HoldChoice;
using loa::settings::ProductKind;
using loa::settings::SignatureStatus;
using loa::settings::Verdict;

namespace {

Version v(std::string_view text) { return *Version::parse(text); }
void put(const Path& path, std::string_view bytes) {
    std::filesystem::create_directories(path.parent_path());
    std::ofstream out(path, std::ios::binary);
    out.write(bytes.data(), static_cast<std::streamsize>(bytes.size()));
}
std::string get(const Path& path) {
    std::ifstream in(path, std::ios::binary);
    return {std::istreambuf_iterator<char>(in), {}};
}
std::string digest(std::string_view bytes, const EVP_MD* md) {
    std::array<std::uint8_t, EVP_MAX_MD_SIZE> hash{};
    unsigned len = 0;
    EVP_Digest(bytes.data(), bytes.size(), hash.data(), &len, md, nullptr);
    return to_hex(std::span(hash.data(), len));
}

// A tar entry: its path and either a symlink target or file contents.
struct Entry {
    std::string path;
    std::string contents;
    bool symlink = false;
};
std::string tgz(const std::vector<Entry>& entries) {
    struct Output {
        std::string bytes;
        static int open(archive*, void*) { return ARCHIVE_OK; }
        static la_ssize_t write(archive*, void* client, const void* data, std::size_t size) {
            static_cast<Output*>(client)->bytes.append(static_cast<const char*>(data), size);
            return static_cast<la_ssize_t>(size);
        }
        static int close(archive*, void*) { return ARCHIVE_OK; }
    } out;
    auto* writer = archive_write_new();
    archive_write_add_filter_gzip(writer);
    archive_write_set_format_pax_restricted(writer);
    archive_write_open(writer, &out, &Output::open, &Output::write, &Output::close);
    for (const auto& e : entries) {
        auto* entry = archive_entry_new();
        archive_entry_set_pathname(entry, e.path.c_str());
        archive_entry_set_perm(entry, 0755);
        archive_entry_set_filetype(entry, e.symlink ? AE_IFLNK : AE_IFREG);
        if (e.symlink) {
            archive_entry_set_symlink(entry, e.contents.c_str());
            archive_entry_set_size(entry, 0);
        } else {
            archive_entry_set_size(entry, e.contents.size());
        }
        archive_write_header(writer, entry);
        if (!e.symlink) archive_write_data(writer, e.contents.data(), e.contents.size());
        archive_entry_free(entry);
    }
    archive_write_close(writer);
    archive_write_free(writer);
    return out.bytes;
}

ProductSpec spec(std::string id, ProductKind kind, Feed feed, std::optional<std::string> asset, Installed installed, Gates gates) {
    Product product{std::move(id), std::move(kind), std::move(feed), PrereleasePolicy::Stable, std::move(asset)};
    return ProductSpec{product, product.id, std::move(installed), {}, std::move(gates)};
}
Feed github(std::string repo) { return Feed::github("acme", std::move(repo)); }

std::string tool(std::string_view version) { return "#!/bin/sh\necho \"tool " + std::string(version) + "\"\n"; }
Path script(const Path& path, std::string_view body) {
    REQUIRE(loa::runtime::exec::write_executable(path, "#!/bin/sh\n" + std::string(body) + "\n"));
    return path;
}

// A fake GitHub and npm feed with ETags and assets, served on localhost.
class FakeFeed {
  public:
    FakeFeed() {
        server_.Get(R"(/repos/acme/([^/]+)/releases)", [&](const auto& req, auto& res) { list(req, res, req.matches[1]); });
        server_.Get(R"(/npm/([^/]+))", [&](const auto& req, auto& res) { list(req, res, req.matches[1]); });
        server_.Get(R"(/files/([^/]+))", [&](const auto& req, auto& res) {
            const auto name = std::string(req.matches[1]);
            std::lock_guard lock(mutex_);
            if (auto it = files_.find(name); it != files_.end()) res.set_content(it->second, "application/octet-stream");
            else res.status = 404;
        });
        const int port = server_.bind_to_any_port("127.0.0.1");
        REQUIRE(port > 0);
        base = "http://127.0.0.1:" + std::to_string(port);
        thread_ = std::thread([this] { server_.listen_after_bind(); });
        for (int i = 0; i < 100 && !server_.is_running(); ++i) std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    ~FakeFeed() {
        server_.stop();
        thread_.join();
    }
    FakeFeed(const FakeFeed&) = delete;

    std::string file(std::string name, std::string bytes) {
        std::lock_guard lock(mutex_);
        files_.insert_or_assign(name, std::move(bytes));
        return base + "/files/" + name;
    }
    void release(std::string repo, std::string version, std::string notes, std::string bytes, std::vector<std::pair<std::string, std::string>> extra = {}) {
        const auto name = repo + "-" + version;
        auto assets = loa::runtime::json::Array{loa::runtime::json::Object{{"name", name}, {"browser_download_url", file(name, bytes)},
            {"size", std::uint64_t(bytes.size())}, {"digest", "sha256:" + digest(bytes, EVP_sha256())}}};
        for (const auto& [n, b] : extra)
            assets.emplace_back(loa::runtime::json::Object{{"name", n}, {"browser_download_url", file(version + "-" + n, b)}});
        auto release = loa::runtime::json::Object{{"tag_name", "v" + version}, {"body", notes}, {"assets", std::move(assets)}};
        std::lock_guard lock(mutex_);
        auto& list = lists_[repo];
        if (!list.is_array()) list = loa::runtime::json::Array{};
        list.as_array()->emplace_back(std::move(release));
    }
    void npm(std::string package, std::string latest, const std::vector<std::tuple<std::string, std::string, bool>>& versions) {
        loa::runtime::json::Object entries;
        for (const auto& [version, bytes, deprecated] : versions) {
            // Npm SRI checksum (`sha512-<base64>`).
            std::array<std::uint8_t, 64> hash{};
            SHA512(reinterpret_cast<const unsigned char*>(bytes.data()), bytes.size(), hash.data());
            std::array<unsigned char, 90> b64{};
            EVP_EncodeBlock(b64.data(), hash.data(), hash.size());
            loa::runtime::json::Object item{{"dist", loa::runtime::json::Object{{"tarball", file(package + "-" + version + ".tgz", bytes)},
                                      {"integrity", "sha512-" + std::string(reinterpret_cast<const char*>(b64.data()))}}}};
            if (deprecated) item.emplace_back("deprecated", "broken");
            entries.emplace_back(version, std::move(item));
        }
        std::lock_guard lock(mutex_);
        lists_[package] = loa::runtime::json::Object{{"dist-tags", loa::runtime::json::Object{{"latest", latest}}}, {"versions", std::move(entries)}};
    }
    std::string base;
    int not_modified = 0;

  private:
    void list(const httplib::Request& req, httplib::Response& res, std::string key) {
        std::lock_guard lock(mutex_);
        auto it = lists_.find(key);
        if (it == lists_.end()) {
            res.status = 404;
            return;
        }
        const std::string body = loa::runtime::json::dump(it->second);
        const std::string etag = "\"" + std::to_string(body.size()) + "\"";
        if (req.has_header("if-none-match") && req.get_header_value("if-none-match") == etag) {
            ++not_modified;
            res.status = 304;
            return;
        }
        res.set_header("etag", etag);
        res.set_content(body, "application/json");
    }
    httplib::Server server_;
    std::thread thread_;
    std::mutex mutex_;
    std::map<std::string, loa::runtime::json::Value> lists_;
    std::map<std::string, std::string> files_;
};

class FileReviewer final : public ChangelogSummarizer {
  public:
    explicit FileReviewer(Path file) : file_(std::move(file)) {}
    PortResult<ChangelogReview> review(const std::string&) override { return parse_review(get(file_)); }

  private:
    Path file_;
};
class FilePorts final : public GatePorts {
  public:
    explicit FilePorts(Path file) : file_(std::move(file)) {}
    std::unique_ptr<Probe> probe(const OmpGates&, std::chrono::milliseconds) override { return {}; }
    std::unique_ptr<ChangelogSummarizer> reviewer(const Path&, const std::string&, std::chrono::milliseconds) override {
        return std::make_unique<FileReviewer>(file_);
    }

  private:
    Path file_;
};

UpdaterConfig setup(const Path& dir, FakeFeed& feed) {
    const auto review = dir / "review.json";
    put(review, R"({"breaking": [], "removed": []})");
    ProductKind harness{ProductKind::Kind::Harness, "tool"};
    ProductKind omp{ProductKind::Kind::Omp};
    auto cfg = UpdaterConfig(dir / "updates", {
        spec("tool", harness, github("tool"), "tool-{version}", Installed::executable(script(dir / "tool", "echo 'tool 1.0.0'")), Gates::changelog()),
        spec("omp", omp, github("omp"), {}, Installed::executable(script(dir / "omp", "echo omp/9.0.0")), Gates::changelog()),
        spec("pkg", ProductKind{ProductKind::Kind::Harness, "pkg"}, Feed::npm("pkg"), {}, Installed::executable(dir / "absent"), Gates::none()),
    });
    cfg.github_api = feed.base;
    cfg.npm_registry = feed.base + "/npm";
    cfg.ports = std::make_shared<FilePorts>(review);
    return cfg;
}

} // namespace

TEST_CASE("harness update end to end") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    FakeFeed feed;
    auto config = setup(dir->path(), feed);
    const auto review = [&](std::string_view removed) { put(dir->path() / "review.json", "{\"breaking\": [], \"removed\": [" + std::string(removed) + "]}"); };
    feed.release("tool", "1.1.0", "- tool: `--fast` flag removed", tool("1.1.0"));
    auto opened = Updater::open(config);
    REQUIRE(opened);
    const auto updater = *opened;
    const auto status = [&](std::string_view id) {
        auto overview = updater.overview();
        return *std::ranges::find_if(overview.products, [&](const auto& p) { return p.id == id; });
    };

    updater.detect();
    auto offer = updater.check("tool");
    REQUIRE(offer);
    REQUIRE(*offer);
    CHECK((*offer)->version == v("1.1.0"));
    CHECK(updater.check("tool")->has_value());
    CHECK(feed.not_modified == 1);
    CHECK(status("tool").current == v("1.0.0"));
    CHECK(status("tool").newest == v("1.1.0"));
    CHECK(status("tool").installed.kind == loa::settings::InstalledStatus::Kind::System);
    CHECK(status("tool").changelog.find("`--fast`") != std::string::npos);

    auto unstaged = updater.gate("tool");
    REQUIRE_FALSE(unstaged);
    CHECK(unstaged.error().kind() == Error::Kind::NotStaged);
    CHECK(status("tool").error->find("not staged") != std::string::npos);
    CHECK(updater.stage("tool") == v("1.1.0"));
    CHECK_FALSE(status("tool").error);

    review("\"tool: `--fast` flag removed\"");
    auto held = updater.gate("tool");
    REQUIRE(held);
    CHECK(held->harmful().size() == 1);
    CHECK(held->harmful()[0]->check == GateCheck::Changelog);
    CHECK(status("tool").held->version == v("1.1.0"));
    REQUIRE(updater.choose("tool", HoldChoice::Wait));
    CHECK_FALSE(status("tool").offer);
    CHECK(status("tool").waiting_past == v("1.1.0"));
    REQUIRE(updater.unpin("tool"));
    REQUIRE(updater.gate("tool"));
    REQUIRE(updater.choose("tool", HoldChoice::PinCurrent));
    CHECK_FALSE(status("tool").offer);
    CHECK(status("tool").pin == v("1.0.0"));
    auto unknown = updater.set_pins({{"nope", v("1.0.0")}});
    CHECK_FALSE(unknown);
    CHECK(unknown.error().kind() == Error::Kind::UnknownProduct);
    REQUIRE(updater.set_pins({}));

    REQUIRE(updater.gate("tool"));
    auto chosen = updater.choose("tool", HoldChoice::UpdateAnyway);
    REQUIRE(chosen);
    CHECK(*chosen == v("1.1.0"));
    TurnGate turns;
    turns.begin();
    auto pending = updater.apply_pending(turns);
    REQUIRE(pending.size() == 1);
    CHECK(pending[0].second->type == SwapOutcome::Type::Deferred);
    turns.end();
    pending = updater.apply_pending(turns);
    REQUIRE(pending.size() == 1);
    CHECK(pending[0].second->type == SwapOutcome::Type::Swapped);
    CHECK_FALSE(pending[0].second->from);
    const auto managed = updater.executable("tool");
    REQUIRE(managed);
    CHECK(managed->string().starts_with((dir->path() / "updates").string()));
    CHECK(status("tool").current == v("1.1.0"));
    CHECK(status("tool").installed.kind == loa::settings::InstalledStatus::Kind::Managed);

    review("");
    feed.release("tool", "1.2.0", "- faster", tool("1.2.0"));
    REQUIRE(updater.check("tool"));
    REQUIRE(updater.stage("tool"));
    auto clean = updater.gate("tool");
    REQUIRE(clean);
    REQUIRE(clean->checks.size() == 1);
    CHECK(clean->checks[0].verdict == Verdict::Pass);
    CHECK(updater.swap("tool", turns) == SwapOutcome::swapped(v("1.1.0"), v("1.2.0")));
    CHECK(updater.rollback("tool", turns) == SwapOutcome::swapped(v("1.2.0"), v("1.1.0")));

    auto package = [](std::string bytes) {
        return tgz({{"package/bin/pkg", std::move(bytes)}, {"package/bin/alias", "pkg", true}});
    };
    feed.npm("pkg", "1.0.0", {{"0.9.0", package("old"), false}, {"1.0.0", package("new"), false},
                                 {"1.1.0", package("x"), true}, {"2.0.0", package("y"), false}});
    auto pkg_offer = updater.check("pkg");
    REQUIRE(pkg_offer);
    REQUIRE(*pkg_offer);
    CHECK((*pkg_offer)->version == v("1.0.0"));
    REQUIRE(updater.stage("pkg"));
    const auto payload = dir->path() / "updates/versions/pkg/1.0.0/payload/bin";
    CHECK(get(payload / "pkg") == "new");
    CHECK(get(payload / "alias") == "new");

    auto reopened = Updater::open(config);
    REQUIRE(reopened);
    const auto persisted = reopened->overview().products.front();
    CHECK(persisted.current == v("1.1.0"));
    CHECK(persisted.offer->version == v("1.2.0"));
    CHECK(persisted.report->is_clean());
    CHECK(persisted.last_check_unix.has_value());
}

TEST_CASE("archive entries cannot escape the payload") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    for (const auto& [i, entries] : std::vector<std::pair<int, std::vector<Entry>>>{
             {0, {{"link", "../../etc/passwd", true}}}, {1, {{"link", "/etc/passwd", true}}},
             {2, {{"a/link", "../b/../../x", true}}}, {3, {{"../outside", "bad"}}}, {4, {{"/outside", "bad"}}}}) {
        const auto artifact = dir->path() / (std::to_string(i) + ".tgz");
        put(artifact, tgz(entries));
        const auto error = unpack::TarGz{}.unpack(artifact, dir->path() / ("payload" + std::to_string(i)));
        REQUIRE_FALSE(error);
        CHECK(error.error().kind() == Error::Kind::UnsafeEntry);
    }
}

TEST_CASE("symlink parents cannot escape to a staged artifact") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto archive = dir->path() / "attack.tgz";
    put(archive, tgz({{"alias", ".", true}, {"alias/launch", "../artifact/tool-1.0.0", true}}));
    const auto error = unpack::TarGz{}.unpack(archive, dir->path() / "unpacked");
    REQUIRE_FALSE(error);
    CHECK(error.error().kind() == Error::Kind::UnsafeEntry);

    const auto root = dir->path() / "updates";
    const auto staged = stage::version_dir(root, "tool", v("1.0.0"));
    put(staged / "artifact/tool-1.0.0", "artifact");
    put(staged / "payload/tool-1.0.0", "artifact");
    StagedManifest manifest{"tool", v("1.0.0"), "tool-1.0.0", digest("artifact", EVP_sha256()), 8, {}};
    put(staged / "staged.json", loa::runtime::json::dump(to_json(manifest)));
    std::filesystem::create_symlink(".", staged / "payload/alias");
    std::filesystem::create_symlink("../artifact/tool-1.0.0", staged / "payload/launch");
    auto product = spec("tool", ProductKind{ProductKind::Kind::Harness, "tool"}, github("tool"), {}, Installed::executable("tool"), Gates::none());
    product.executable = "alias/launch";
    auto updater = Updater::open(UpdaterConfig(root, {product}));
    REQUIRE(updater);
    auto swapped = Swapper(root).try_swap("tool", v("1.0.0"), std::nullopt, TurnGate{});
    REQUIRE(swapped);
    CHECK_FALSE(updater->executable("tool"));
}

TEST_CASE("app updates need a trusted Rust signature") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const Path golden = Path(LOA_GOLDEN_DIR) / "updater/sig";
    const auto trusted = signature::parse_public_key(get(golden / "trusted.hex"));
    const auto stranger = signature::parse_public_key(get(golden / "stranger.hex"));
    REQUIRE(trusted);
    REQUIRE(stranger);
    FakeFeed feed;
    auto config = setup(dir->path(), feed);
    config.products = {spec("app", ProductKind{ProductKind::Kind::App}, github("app"), "app-{version}", Installed::running(v("0.1.0")), Gates::none())};
    config.trusted_keys = {*trusted};
    const auto bytes = get(golden / "app-0.2.0");
    feed.release("app", "0.2.0", "- faster", bytes, {{"app-0.2.0.sig", get(golden / "app-0.2.0.sig")}});
    auto updater = Updater::open(config);
    REQUIRE(updater);
    REQUIRE(updater->check("app"));
    REQUIRE(updater->stage("app"));
    auto report = updater->gate("app");
    REQUIRE(report);
    CHECK(report->checks.empty());
    CHECK(updater->overview().products[0].signature->kind == SignatureStatus::Kind::Verified);
    auto outcome = updater->swap("app", TurnGate{});
    REQUIRE(outcome);
    CHECK(outcome->type == SwapOutcome::Type::Swapped);
    CHECK(updater->overview().products[0].restart_required);
    CHECK(updater->overview().products[0].current == v("0.1.0"));

    const auto stage_fixture = [&](std::string version, std::string content, std::optional<std::string> sig) {
        const auto dir = stage::version_dir(config.root, "app", v(version));
        const auto name = "app-" + version;
        put(dir / "artifact" / name, content);
        put(dir / "payload" / name, content);
        if (sig) put(dir / "artifact" / (name + ".sig"), *sig);
        StagedManifest manifest{"app", v(version), name, digest(content, EVP_sha256()), content.size(), sig ? std::optional(name + ".sig") : std::nullopt};
        put(dir / "staged.json", loa::runtime::json::dump(to_json(manifest)));
    };
    stage_fixture("0.3.0", "unsigned", {});
    // A correct signature under another key, computed by OpenSSL in the test.
    auto pkey = std::unique_ptr<EVP_PKEY, decltype(&EVP_PKEY_free)>(EVP_PKEY_new_raw_private_key(EVP_PKEY_ED25519, nullptr,
                                            reinterpret_cast<const unsigned char*>(std::string(32, '\x09').data()), 32), &EVP_PKEY_free);
    std::array<std::uint8_t, 64> sig{};
    std::size_t len = sig.size();
    auto ctx = std::unique_ptr<EVP_MD_CTX, decltype(&EVP_MD_CTX_free)>(EVP_MD_CTX_new(), &EVP_MD_CTX_free);
    EVP_DigestSignInit(ctx.get(), nullptr, nullptr, nullptr, pkey.get());
    std::array<std::uint8_t, 32> hash{};
    SHA256(reinterpret_cast<const unsigned char*>("stranger"), 8, hash.data());
    EVP_DigestSign(ctx.get(), sig.data(), &len, hash.data(), hash.size());
    stage_fixture("0.4.0", "stranger", to_hex(sig) + "\n");
    const auto swapper = Swapper(config.root);
    const auto unsigned_outcome = swapper.try_swap("app", v("0.3.0"), std::span<const VerifyingKey>(config.trusted_keys), TurnGate{});
    REQUIRE_FALSE(unsigned_outcome);
    CHECK(unsigned_outcome.error().kind() == Error::Kind::Unsigned);
    const auto untrusted = swapper.try_swap("app", v("0.4.0"), std::span<const VerifyingKey>(config.trusted_keys), TurnGate{});
    REQUIRE_FALSE(untrusted);
    CHECK(untrusted.error().kind() == Error::Kind::Signature);
    const auto unconfigured = swapper.try_swap("app", v("0.4.0"), std::nullopt, TurnGate{});
    REQUIRE_FALSE(unconfigured);
    CHECK(unconfigured.error().kind() == Error::Kind::Signature);
}

TEST_CASE("a damaged feed cache is dropped without discarding updater state") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    FakeFeed feed;
    auto config = setup(dir->path(), feed);
    put(config.root / "feeds.json", "{broken");
    UpdaterState state;
    state.pins["tool"] = v("1.1.0");
    REQUIRE(state.save(config.root));
    auto updater = Updater::open(config);
    REQUIRE(updater);
    CHECK_FALSE(updater->overview().products[0].offer);
    feed.release("tool", "1.1.0", "hello", tool("1.1.0"));
    auto offer = updater->check("tool");
    REQUIRE(offer);
    REQUIRE(*offer);
    CHECK((*offer)->version == v("1.1.0"));
    CHECK(updater->overview().products[0].pin == v("1.1.0"));
}

TEST_CASE("HTTP follows redirects and feed reports rate limits") {
    httplib::Server server;
    server.Get("/redirect", [](const auto&, auto& res) {
        res.status = 302;
        res.set_header("Location", "/file");
        res.set_content("redirect body that must not enter the artifact", "text/plain");
    });
    server.Get("/file", [](const auto&, auto& res) { res.set_content("final bytes", "application/octet-stream"); });
    server.Get("/repos/acme/limit/releases", [](const auto&, auto& res) {
        res.status = 403;
        res.set_header("x-ratelimit-remaining", "0");
        res.set_header("x-ratelimit-reset", "1760000000");
    });
    server.Get("/repos/acme/retry/releases", [](const auto&, auto& res) {
        res.status = 429;
        res.set_header("retry-after", "60");
    });
    const auto port = server.bind_to_any_port("127.0.0.1");
    REQUIRE(port > 0);
    std::thread thread([&] { server.listen_after_bind(); });
    const auto base = "http://127.0.0.1:" + std::to_string(port);
    auto stop = [&] {
        server.stop();
        thread.join();
    };
    const auto response = Http{}.get(base + "/redirect");
    CHECK(response);
    if (response) {
        CHECK(response->status == 200);
        CHECK(response->body == "final bytes");
    }
    Product product{"tool", ProductKind{ProductKind::Kind::Harness, "tool"}, github("limit"), {}, std::nullopt};
    auto feed = feeds::GitHub(Http{}, base);
    auto limited = feed.fetch(product, nullptr);
    REQUIRE_FALSE(limited);
    CHECK(limited.error().kind() == Error::Kind::RateLimited);
    CHECK(limited.error().retry_at() == 1'760'000'000);
    product.feed = github("retry");
    auto retry = feed.fetch(product, nullptr);
    REQUIRE_FALSE(retry);
    CHECK(retry.error().kind() == Error::Kind::RateLimited);
    CHECK(retry.error().retry_at() > 1'760'000'000);
    stop();
}

TEST_CASE("zip path traversal is rejected") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto artifact = dir->path() / "unsafe.zip";
    auto* writer = archive_write_new();
    archive_write_set_format_zip(writer);
    REQUIRE(archive_write_open_filename(writer, artifact.c_str()) == ARCHIVE_OK);
    auto* entry = archive_entry_new();
    archive_entry_set_pathname(entry, "../escape");
    archive_entry_set_filetype(entry, AE_IFREG);
    archive_entry_set_perm(entry, 0644);
    archive_entry_set_size(entry, 4);
    REQUIRE(archive_write_header(writer, entry) == ARCHIVE_OK);
    REQUIRE(archive_write_data(writer, "evil", 4) == 4);
    archive_entry_free(entry);
    archive_write_close(writer);
    archive_write_free(writer);
    auto result = unpack::Zip{}.unpack(artifact, dir->path() / "payload");
    REQUIRE_FALSE(result);
    CHECK(result.error().kind() == Error::Kind::UnsafeEntry);
}

TEST_CASE("swap keeps the previous version for rollback and prunes older versions") {
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir);
    const auto root = dir->path() / "updates";
    for (const auto* version : {"1.0.0", "1.1.0", "1.2.0", "1.3.0"}) {
        const auto staged = stage::version_dir(root, "tool", v(version));
        const auto name = std::string("tool-") + version;
        put(staged / "artifact" / name, tool(version));
        put(staged / "payload" / name, tool(version));
        const StagedManifest manifest{"tool", v(version), name, digest(tool(version), EVP_sha256()), tool(version).size(), {}};
        put(staged / "staged.json", loa::runtime::json::dump(to_json(manifest)));
    }
    Swapper swapper(root);
    swapper.keep = 1;
    TurnGate idle;
    CHECK(swapper.try_swap("tool", v("1.0.0"), std::nullopt, idle) == SwapOutcome::swapped({}, v("1.0.0")));
    CHECK(swapper.try_swap("tool", v("1.1.0"), std::nullopt, idle) == SwapOutcome::swapped(v("1.0.0"), v("1.1.0")));
    CHECK(swapper.try_swap("tool", v("1.2.0"), std::nullopt, idle) == SwapOutcome::swapped(v("1.1.0"), v("1.2.0")));
    CHECK(swapper.try_swap("tool", v("1.3.0"), std::nullopt, idle) == SwapOutcome::swapped(v("1.2.0"), v("1.3.0")));
    CHECK_FALSE(std::filesystem::exists(stage::version_dir(root, "tool", v("1.0.0"))));
    CHECK(std::filesystem::exists(stage::version_dir(root, "tool", v("1.1.0"))));
    CHECK(get(stage::product_dir(root, "tool") / "active.json") ==
          loa::runtime::json::dump_pretty(loa::settings::to_json(loa::settings::Active{v("1.3.0"), v("1.2.0")})));
    CHECK(swapper.rollback("tool", std::nullopt, idle) == SwapOutcome::swapped(v("1.3.0"), v("1.2.0")));
    CHECK(swapper.active("tool")->value().previous == v("1.3.0"));
}
