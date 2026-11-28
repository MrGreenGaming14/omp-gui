#include "loa/updater/service.hpp"

#include <algorithm>
#include <filesystem>
#include <fstream>
#include <future>
#include <mutex>

#include "loa/runtime/exec.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/runtime/time.hpp"
#include "loa/updater/decision.hpp"
#include "loa/updater/feeds.hpp"
#include "loa/updater/stage.hpp"
#include "loa/updater/state.hpp"
#include "util.hpp"

namespace loa::updater {

namespace stdfs = std::filesystem;
using loa::settings::Active;
using loa::settings::CheckResult;
using loa::settings::GateCheck;
using loa::settings::HoldChoice;
using loa::settings::InstalledStatus;
using loa::settings::Overview;
using loa::settings::ProductStatus;
using loa::settings::RateLimit;
using loa::settings::SignatureStatus;

std::string platform() {
#if defined(__APPLE__)
    const std::string os = "darwin";
#elif defined(_WIN32)
    const std::string os = "windows";
#elif defined(__linux__)
    const std::string os = "linux";
#else
    const std::string os = "unknown";
#endif
#if defined(__x86_64__) || defined(_M_X64)
    const std::string arch = "x64";
#elif defined(__aarch64__) || defined(_M_ARM64)
    const std::string arch = "arm64";
#else
    const std::string arch = "unknown";
#endif
    return os + "-" + arch;
}

std::vector<ProductSpec> default_products(const Version& app_version) {
    struct Row {
        const char* id;
        const char* label;
        const char* feed;
        const char* asset;
        const char* executable;
    };
    // `(id, label, feed, asset, executable)`; `gh:` feeds are `owner/repo`, `npm:` a package.
    // `app` is the running app, `omp` gets every gate, the rest are harnesses with a changelog gate.
    static constexpr Row CATALOG[] = {
        {"app", "loa", "gh:MrGreenGaming14/omp-gui", "loa-app-{platform}.tar.gz", "loa-app"},
        {"omp", "omp", "gh:can1357/oh-my-pi", "omp-{platform}", nullptr},
        {"claude", "Claude Code", "npm:@anthropic-ai/claude-code", nullptr, nullptr},
        {"codex", "Codex", "npm:@openai/codex", nullptr, nullptr},
        {"opencode", "OpenCode", "npm:opencode-ai", nullptr, nullptr},
    };
    std::vector<ProductSpec> products;
    for (const auto& row : CATALOG) {
        const std::string id = row.id;
        const std::string feed = row.feed;
        Feed source;
        if (feed.starts_with("gh:")) {
            const auto name = feed.substr(3);
            const auto slash = name.find('/');
            source = Feed::github(name.substr(0, slash), slash == std::string::npos ? "" : name.substr(slash + 1));
        } else {
            source = Feed::npm(feed.substr(4));
        }
        ProductKind kind;
        Installed installed;
        Gates gates;
        if (id == "app") {
            kind.kind = ProductKind::Kind::App;
            installed = Installed::running(app_version);
            gates = Gates::none();
        } else if (id == "omp") {
            kind.kind = ProductKind::Kind::Omp;
            installed = Installed::executable(id);
            gates = Gates::omp_gates(OmpGates::defaults());
        } else {
            kind.kind = ProductKind::Kind::Harness;
            kind.harness = id;
            installed = Installed::executable(id);
            gates = Gates::changelog();
        }
        std::optional<std::string> asset;
        if (row.asset) {
            asset = row.asset;
            if (auto at = asset->find("{platform}"); at != std::string::npos) asset->replace(at, 10, platform());
        }
        Product product{id, std::move(kind), std::move(source), PrereleasePolicy::Stable, std::move(asset)};
        products.push_back(ProductSpec{std::move(product), row.label, std::move(installed),
                                       row.executable ? std::optional<std::string>(row.executable) : std::nullopt, std::move(gates)});
    }
    return products;
}

void TurnGate::end() noexcept {
    auto n = running_.load();
    while (n > 0 && !running_.compare_exchange_weak(n, n - 1)) {}
}

std::expected<Version, std::string> executable_version(const Path& executable, std::chrono::milliseconds timeout) {
    auto text = loa::runtime::exec::probe_cli(loa::runtime::exec::resolve(executable), {"--version"}, timeout);
    if (!text) return std::unexpected(text.error().to_string());
    if (auto candidate = loa::runtime::exec::parse_version(*text)) {
        auto v = Version::parse(*candidate);
        if (v) return *v;
    }
    std::string trimmed = *text;
    while (!trimmed.empty() && std::isspace(static_cast<unsigned char>(trimmed.back()))) trimmed.pop_back();
    while (!trimmed.empty() && std::isspace(static_cast<unsigned char>(trimmed.front()))) trimmed.erase(trimmed.begin());
    return std::unexpected("no version in " + debug_str(trimmed));
}

namespace {

using Detected = std::expected<std::pair<Version, Path>, std::string>;

// The version a product uses now and its executable.
struct Current {
    Version version;
    std::optional<Path> executable;
};

Result<std::map<std::string, CachedFeed>> decode_cache(const detail::Json& v) {
    auto decoded = detail::decoding([&] {
        std::map<std::string, CachedFeed> out;
        for (const auto& [k, feed] : detail::object(v, "a map")) out.insert_or_assign(k, detail::value(cached_feed_from_json(feed)));
        return out;
    });
    if (!decoded) return std::unexpected(Error::feed(decoded.error()));
    return std::move(*decoded);
}

detail::Json cache_json(const std::map<std::string, CachedFeed>& cache) {
    detail::Object out;
    for (const auto& [key, feed] : cache) out.emplace_back(key, to_json(feed));
    return out;
}

Result<Path> staged_executable(const ProductSpec& spec, const Staged& staged) {
    const auto& version = staged.manifest.version;
    const auto payload = staged.payload();
    const auto none = [&](std::string reason) { return Error::no_executable(spec.product.id, version, reason); };
    Path path;
    if (spec.executable) {
        std::string pattern = *spec.executable;
        for (std::size_t at = pattern.find("{version}"); at != std::string::npos; at = pattern.find("{version}", at + version.to_string().size()))
            pattern.replace(at, 9, version.to_string());
        path = payload / pattern;
    } else if (stdfs::is_regular_file(payload / staged.manifest.artifact_name)) {
        path = payload / staged.manifest.artifact_name;
    } else if (stdfs::is_regular_file(payload / "package.json")) {
        const auto package = payload / "package.json";
        std::ifstream file(package, std::ios::binary);
        if (!file) return std::unexpected(none("reading " + package.string() + ": " + std::generic_category().message(errno)));
        std::string text((std::istreambuf_iterator<char>(file)), {});
        auto json = loa::runtime::json::parse(text);
        if (!json) return std::unexpected(none(package.string() + ": " + json.error().message()));
        const auto* bin = json->find("bin");
        std::optional<std::string> name;
        if (bin) {
            if (auto* str = bin->as_string()) name = *str;
            else if (auto* map = bin->as_object()) {
                for (const auto& [k, v] : *map)
                    if (k == spec.product.id && v.as_string()) name = *v.as_string();
                if (!name && map->size() == 1 && map->front().second.as_string()) name = *map->front().second.as_string();
            }
        }
        if (!name) return std::unexpected(none("package.json names no executable"));
        std::string_view rest = *name;
        for (std::size_t start = 0; start <= rest.size();) {
            const auto end = rest.find_first_of("/\\", start);
            if (rest.substr(start, end - start) == "..") return std::unexpected(none("package.json bin " + debug_str(*name) + " leaves the package"));
            if (end == std::string_view::npos) break;
            start = end + 1;
        }
        path = payload / *name;
    } else {
        return std::unexpected(none("the payload is neither a single file nor an npm package"));
    }
    const auto rel = path.lexically_relative(payload);
    if (rel.empty()) return std::unexpected(none(path.string() + " is not a file in the payload"));
    auto resolved = loa::runtime::fs::confine(payload, rel);
    if (!resolved || !stdfs::is_regular_file(*resolved)) return std::unexpected(none(path.string() + " is not a file in the payload"));
    return path;
}

std::vector<Version> staged_versions(const Path& root, std::string_view product) {
    std::vector<Version> versions;
    std::error_code ec;
    for (stdfs::directory_iterator it(stage::product_dir(root, product), ec), end; !ec && it != end; it.increment(ec)) {
        if (!stdfs::is_regular_file(it->path() / std::string(MANIFEST_FILE), ec)) continue;
        if (auto v = Version::parse(it->path().filename().string())) versions.push_back(*v);
    }
    std::ranges::sort(versions);
    return versions;
}

} // namespace

struct Updater::Inner {
    explicit Inner(UpdaterConfig config, UpdaterState state, std::map<std::string, CachedFeed> cache)
        : config(std::move(config)),
          feeds({{"github", std::make_shared<feeds::GitHub>(Http{}, this->config.github_api, this->config.github_token)},
                 {"npm", std::make_shared<feeds::Npm>(Http{}, this->config.npm_registry)}}),
          stager(Http{}, this->config.root),
          swapper(this->config.root),
          state(std::move(state)),
          cache(std::move(cache)) {
        for (const auto& p : this->config.products)
            if (p.product.kind.kind == ProductKind::Kind::App) {
                swapper.app_product = p.product.id;
                break;
            }
    }

    UpdaterConfig config;
    std::map<std::string, std::shared_ptr<feeds::FeedSource>> feeds;
    Stager stager;
    Swapper swapper;
    // Like the Rust service, these maps can be queried and updated from concurrent callers.
    mutable std::mutex mutex;
    UpdaterState state;
    std::map<std::string, CachedFeed> cache;
    std::map<std::string, Detected> installed;
    std::map<std::pair<std::string, Version>, SignatureStatus> signatures;
    std::map<std::string, std::string> errors;
    std::optional<RateLimit> rate_limit;

    Result<const ProductSpec*> spec(std::string_view id) const {
        for (const auto& s : config.products)
            if (s.product.id == id) return &s;
        return std::unexpected(Error::unknown_product(id));
    }

    template <typename T>
    Result<T> note(std::string_view id, Result<T> result) {
        std::lock_guard lock(mutex);
        if (result) errors.erase(std::string(id));
        else errors.insert_or_assign(std::string(id), result.error().to_string());
        return result;
    }

    Result<std::optional<Current>> current(const ProductSpec& spec) const {
        const auto& id = spec.product.id;
        if (spec.installed.type == Installed::Type::Running) return std::optional<Current>{Current{spec.installed.version, std::nullopt}};
        auto active = swapper.active(id);
        if (!active) return std::unexpected(active.error());
        if (*active) {
            std::optional<Path> executable;
            auto staged = stage::staged(config.root, id, (**active).current);
            if (!staged) return std::unexpected(staged.error());
            if (!*staged) return std::unexpected(Error::not_staged(id, (**active).current));
            if (auto path = staged_executable(spec, **staged)) executable = std::move(*path);
            return std::optional<Current>{Current{(**active).current, std::move(executable)}};
        }
        std::lock_guard lock(mutex);
        if (auto it = installed.find(id); it != installed.end() && it->second)
            return std::optional<Current>{Current{it->second->first, it->second->second}};
        return std::optional<Current>{};
    }

    Result<std::optional<Current>> current_or_detect(const ProductSpec& spec, const Updater& updater) const {
        if (spec.installed.type == Installed::Type::Executable) {
            bool detected;
            {
                std::lock_guard lock(mutex);
                detected = installed.contains(spec.product.id);
            }
            if (!detected) {
                auto active = current(spec);
                if (!active) return std::unexpected(active.error());
                if (*active) return active;
                updater.detect();
            }
        }
        return current(spec);
    }

    Result<Staged> staged(std::string_view id, const Version& version) const {
        auto found = stage::staged(config.root, id, version);
        if (!found) return std::unexpected(found.error());
        if (!*found) return std::unexpected(Error::not_staged(id, version));
        return std::move(**found);
    }

    std::optional<Release> offer(const ProductSpec& spec, const Version& current) const {
        std::lock_guard lock(mutex);
        const auto& id = spec.product.id;
        const auto feed = cache.find(feeds::cache_key(spec.product));
        if (feed == cache.end()) return std::nullopt;
        std::vector<Release> allowed;
        for (const auto& r : feed->second.releases)
            if (decision::offer_allowed(state, id, r.version)) allowed.push_back(r);
        const auto pin = state.pins.find(id);
        const auto* release = feeds::newest(allowed, current, spec.product.prereleases, pin == state.pins.end() ? nullptr : &pin->second);
        return release ? std::optional<Release>(*release) : std::nullopt;
    }

    Result<Release> offered(const ProductSpec& spec, const Updater& updater) const {
        auto current = current_or_detect(spec, updater);
        if (!current) return std::unexpected(current.error());
        const auto version = *current ? (**current).version : Version{};
        auto release = offer(spec, version);
        if (!release) return std::unexpected(Error::nothing_offered(spec.product.id));
        return std::move(*release);
    }

    std::string missing_reason(std::string_view id) const {
        std::lock_guard lock(mutex);
        if (auto it = installed.find(std::string(id)); it != installed.end() && !it->second) return it->second.error();
        return "no version found";
    }

    std::string changelog(const ProductSpec& spec, const Version& from, const Version& to) const {
        std::lock_guard lock(mutex);
        auto it = cache.find(feeds::cache_key(spec.product));
        if (it == cache.end()) return {};
        std::vector<std::pair<Version, std::string>> notes;
        for (const auto* r : feeds::between(it->second.releases, from, to)) {
            if (r->notes.find_first_not_of(" \t\n\r") != std::string::npos) notes.emplace_back(r->version, r->notes);
        }
        return notes.empty() ? std::string{} : changelog_text(notes);
    }

    std::optional<std::span<const VerifyingKey>> trusted() const {
        if (config.trusted_keys.empty()) return std::nullopt;
        return std::span<const VerifyingKey>(config.trusted_keys);
    }

    SignatureStatus verify_staged(std::string_view id, Staged staged) {
        const auto version = staged.manifest.version;
        SignatureStatus status;
        if (config.trusted_keys.empty()) {
            status.kind = SignatureStatus::Kind::NoTrustedKeys;
        } else {
            auto r = signature::verify_staged(staged, config.trusted_keys);
            if (r) status.kind = SignatureStatus::Kind::Verified;
            else if (r.error().kind() == Error::Kind::Unsigned) status.kind = SignatureStatus::Kind::Unsigned;
            else {
                status.kind = SignatureStatus::Kind::Rejected;
                status.reason = r.error().to_string();
            }
        }
        std::lock_guard lock(mutex);
        signatures.insert_or_assign({std::string(id), version}, status);
        return status;
    }

    GateReport run_checks(const ProductSpec& spec, const Gates& gates, const Current& current, const Path& next, const Version& to,
                          const Updater& updater) const {
        const auto& id = spec.product.id;
        const auto report = [&](std::vector<CheckResult> checks) { return GateReport{id, current.version, to, std::move(checks)}; };
        const auto notes = changelog(spec, current.version, to);
        // Reviews run on the omp in use, never on an unchecked staged one.
        std::optional<Path> reviewer_exe = id == config.reviewer ? current.executable : updater.executable(config.reviewer);
        std::unique_ptr<ChangelogSummarizer> reviewer;
        if (config.ports && reviewer_exe) reviewer = config.ports->reviewer(*reviewer_exe, config.review_model, config.review_timeout);
        std::optional<CheckResult> note;
        if (notes.find_first_not_of(" \t\n\r") == std::string::npos) {
            note = CheckResult::pass(GateCheck::Changelog, {"no release notes published after " + current.version.to_string()});
        } else if (!reviewer) {
            note = CheckResult::failed_to_run(GateCheck::Changelog, config.reviewer + " is not installed to review it");
        }
        std::unique_ptr<Probe> probe;
        const OmpGates* omp = nullptr;
        if (gates.type == Gates::Type::Omp) {
            if (!current.executable)
                return report({CheckResult::failed_to_run(GateCheck::ToolSchemas, "the version in use has no executable to compare with")});
            if (!config.ports)
                return report({CheckResult::failed_to_run(GateCheck::ToolSchemas, "no gate adapters are configured")});
            probe = config.ports->probe(gates.omp, config.probe_timeout);
            omp = &gates.omp;
        }
        GateInputs inputs{current.executable.value_or(Path{}), next, probe.get(), omp, note ? nullptr : reviewer.get(), notes};
        auto result = run_gates(id, current.version, to, inputs);
        if (note) result.checks.push_back(std::move(*note));
        return result;
    }

    ProductStatus status(const ProductSpec& spec) const {
        const auto& id = spec.product.id;
        ProductStatus s;
        s.id = id;
        s.label = spec.label;
        s.kind = spec.product.kind;
        s.source = spec.product.feed.type == Feed::Type::GitHub ? "GitHub " + spec.product.feed.owner + "/" + spec.product.feed.repo
                                                               : "npm " + spec.product.feed.package;
        {
            std::lock_guard lock(mutex);
            if (auto it = errors.find(id); it != errors.end()) s.error = it->second;
        }
        auto active = swapper.active(id);
        if (!active && !s.error) s.error = active.error().to_string();
        if (active) s.active = *active;
        auto current = this->current(spec);
        const auto c = current && *current ? **current : Current{};
        if (current && *current) s.current = c.version;
        if (spec.installed.type == Installed::Type::Running) {
            s.installed.kind = InstalledStatus::Kind::Running;
        } else if (s.active) {
            if (c.executable) {
                s.installed.kind = InstalledStatus::Kind::Managed;
                s.installed.path = *c.executable;
            } else {
                s.installed.kind = InstalledStatus::Kind::Missing;
                s.installed.reason = "the active version has no executable";
            }
        } else {
            std::lock_guard lock(mutex);
            if (auto it = installed.find(id); it != installed.end()) {
                if (it->second) {
                    s.installed.kind = InstalledStatus::Kind::System;
                    s.installed.path = it->second->second;
                } else {
                    s.installed.kind = InstalledStatus::Kind::Missing;
                    s.installed.reason = it->second.error();
                }
            }
        }
        const auto version = s.current.value_or(Version{});
        s.offer = offer(spec, version);
        {
            std::lock_guard lock(mutex);
            const auto key = feeds::cache_key(spec.product);
            if (auto it = cache.find(key); it != cache.end()) {
                for (const auto& r : it->second.releases) {
                    if (spec.product.prereleases == PrereleasePolicy::Stable && !r.version.pre.empty()) continue;
                    if (!s.newest || r.version > *s.newest) s.newest = r.version;
                }
            }
        }
        if (s.offer) s.changelog = changelog(spec, version, s.offer->version);
        {
            std::lock_guard lock(mutex);
            if (s.offer) {
                if (auto it = signatures.find({id, s.offer->version}); it != signatures.end()) s.signature = it->second;
            }
            if (auto it = state.last_check_unix.find(id); it != state.last_check_unix.end()) s.last_check_unix = it->second;
            if (auto it = state.reports.find(id); it != state.reports.end()) s.report = it->second;
            if (auto it = state.held.find(id); it != state.held.end()) s.held = it->second;
            if (auto it = state.pending_swap.find(id); it != state.pending_swap.end()) s.pending_swap = it->second;
            if (auto it = state.pins.find(id); it != state.pins.end()) s.pin = it->second;
            if (auto it = state.waits.find(id); it != state.waits.end()) s.waiting_past = it->second;
        }
        s.staged = staged_versions(config.root, id);
        s.restart_required = spec.installed.type == Installed::Type::Running && s.active && s.active->current != spec.installed.version;
        return s;
    }
};

Result<Updater> Updater::open(UpdaterConfig config) {
    auto state = UpdaterState::load(config.root);
    if (!state) return std::unexpected(state.error());
    // The cache only saves requests; a damaged one is dropped.
    auto cache_file = detail::read_json<std::map<std::string, CachedFeed>>(config.root / "feeds.json", [](const detail::Json& v) {
        auto r = decode_cache(v);
        if (!r) return detail::Decoded<std::map<std::string, CachedFeed>>(std::unexpected(r.error().message()));
        return detail::Decoded<std::map<std::string, CachedFeed>>(std::move(*r));
    });
    if (!cache_file && cache_file.error().kind() != Error::Kind::Json) return std::unexpected(cache_file.error());
    return Updater(std::make_shared<Inner>(std::move(config), std::move(*state), cache_file ? cache_file->value_or(std::map<std::string, CachedFeed>{})
                                                                                          : std::map<std::string, CachedFeed>{}));
}

const UpdaterConfig& Updater::config() const { return inner_->config; }

void Updater::detect() const {
    // Concurrent CLI probes, then update the installed map in one lock.
    std::vector<std::future<std::pair<std::string, Detected>>> running;
    for (const auto& spec : inner_->config.products) {
        if (spec.installed.type != Installed::Type::Executable) continue;
        running.push_back(std::async(std::launch::async, [id = spec.product.id, path = spec.installed.path,
                                                         timeout = inner_->config.probe_timeout] {
            const auto found = loa::runtime::exec::which(path);
            if (!found) return std::pair<std::string, Detected>{id, std::unexpected(path.string() + " not found")};
            auto version = executable_version(*found, timeout);
            if (!version) return std::pair<std::string, Detected>{id, std::unexpected(version.error())};
            return std::pair<std::string, Detected>{id, std::pair{*version, *found}};
        }));
    }
    std::lock_guard lock(inner_->mutex);
    for (auto& f : running) {
        auto [id, result] = f.get();
        inner_->installed.insert_or_assign(std::move(id), std::move(result));
    }
}

std::optional<Path> Updater::executable(std::string_view id) const {
    auto spec = inner_->spec(id);
    if (!spec) return std::nullopt;
    auto current = inner_->current(**spec);
    return current && *current ? (**current).executable : std::nullopt;
}

Result<std::optional<Release>> Updater::check(std::string_view id) const {
    const std::string product(id);
    const auto work = [&]() -> Result<std::optional<Release>> {
        auto spec = inner_->spec(id);
        if (!spec) return std::unexpected(spec.error());
        const auto key = feeds::cache_key((**spec).product);
        const auto source = inner_->feeds.find(std::string((**spec).product.feed.kind()));
        if (source == inner_->feeds.end()) return std::unexpected(Error::feed("no source for " + key));
        std::optional<CachedFeed> cached;
        {
            std::lock_guard lock(inner_->mutex);
            if (auto it = inner_->cache.find(key); it != inner_->cache.end()) cached = it->second;
        }
        auto response = source->second->fetch((**spec).product, cached ? &*cached : nullptr);
        if (!response) return std::unexpected(response.error());
        if (response->not_modified && !cached) return std::unexpected(Error::feed(key + " answered 304 without a cached response"));
        if (!response->not_modified) {
            std::lock_guard lock(inner_->mutex);
            if (response->rate_limit) inner_->rate_limit = response->rate_limit;
            inner_->cache.insert_or_assign(key, std::move(response->feed));
            if (auto r = detail::write_json(inner_->config.root / "feeds.json", cache_json(inner_->cache)); !r)
                return std::unexpected(r.error());
        }
        {
            std::lock_guard lock(inner_->mutex);
            inner_->state.last_check_unix.insert_or_assign(product, loa::runtime::time::now_secs());
            if (auto r = inner_->state.save(inner_->config.root); !r) return std::unexpected(r.error());
        }
        auto release = inner_->offered(**spec, *this);
        if (!release) {
            if (release.error().kind() == Error::Kind::NothingOffered) return std::optional<Release>{};
            return std::unexpected(release.error());
        }
        return std::optional<Release>{std::move(*release)};
    };
    return inner_->note(product, work());
}

std::vector<std::pair<std::string, Result<std::optional<Release>>>> Updater::check_all() const {
    std::vector<std::future<std::pair<std::string, Result<std::optional<Release>>>>> running;
    for (const auto& spec : inner_->config.products)
        running.push_back(std::async(std::launch::async, [this, id = spec.product.id] { return std::pair{id, check(id)}; }));
    std::vector<std::pair<std::string, Result<std::optional<Release>>>> out;
    for (auto& f : running) out.push_back(f.get());
    return out;
}

Result<Version> Updater::stage(std::string_view id) const {
    const auto work = [&]() -> Result<Version> {
        auto spec = inner_->spec(id);
        if (!spec) return std::unexpected(spec.error());
        auto release = inner_->offered(**spec, *this);
        if (!release) return std::unexpected(release.error());
        auto staged = inner_->stager.stage(*release);
        if (!staged) return std::unexpected(staged.error());
        if ((**spec).product.kind.kind == ProductKind::Kind::App) inner_->verify_staged(id, *staged);
        return release->version;
    };
    return inner_->note(id, work());
}

Result<SignatureStatus> Updater::verify(std::string_view id) const {
    auto spec = inner_->spec(id);
    if (!spec) return std::unexpected(spec.error());
    auto release = inner_->offered(**spec, *this);
    if (!release) return std::unexpected(release.error());
    auto staged = inner_->staged(id, release->version);
    if (!staged) return std::unexpected(staged.error());
    return inner_->verify_staged(id, *staged);
}

Result<GateReport> Updater::gate(std::string_view id) const {
    const auto work = [&]() -> Result<GateReport> {
        auto spec = inner_->spec(id);
        if (!spec) return std::unexpected(spec.error());
        auto current = inner_->current_or_detect(**spec, *this);
        if (!current) return std::unexpected(current.error());
        if (!*current) return std::unexpected(Error::not_installed(id, inner_->missing_reason(id)));
        auto release = inner_->offer(**spec, (**current).version);
        if (!release) return std::unexpected(Error::nothing_offered(id));
        auto staged = inner_->staged(id, release->version);
        if (!staged) return std::unexpected(staged.error());
        GateReport report;
        if ((**spec).gates.type == Gates::Type::None) {
            report = GateReport{std::string(id), (**current).version, release->version, {}};
        } else {
            auto next = staged_executable(**spec, *staged);
            if (!next) return std::unexpected(next.error());
            report = inner_->run_checks(**spec, (**spec).gates, **current, *next, release->version, *this);
        }
        std::lock_guard lock(inner_->mutex);
        decision::record(inner_->state, report, loa::runtime::time::now_secs());
        inner_->state.reports.insert_or_assign(std::string(id), report);
        if (auto r = inner_->state.save(inner_->config.root); !r) return std::unexpected(r.error());
        return report;
    };
    return inner_->note(id, work());
}

Result<std::optional<Version>> Updater::choose(std::string_view id, HoldChoice choice) const {
    const auto work = [&]() -> Result<std::optional<Version>> {
        auto spec = inner_->spec(id);
        if (!spec) return std::unexpected(spec.error());
        auto current = inner_->current(**spec);
        if (!current) return std::unexpected(current.error());
        if (!*current) return std::unexpected(Error::not_installed(id, "no version in use"));
        std::lock_guard lock(inner_->mutex);
        auto to = decision::apply_choice(inner_->state, id, (**current).version, choice);
        if (auto r = inner_->state.save(inner_->config.root); !r) return std::unexpected(r.error());
        return to;
    };
    return inner_->note(id, work());
}

Result<> Updater::unpin(std::string_view id) const {
    auto spec = inner_->spec(id);
    if (!spec) return std::unexpected(spec.error());
    std::lock_guard lock(inner_->mutex);
    inner_->state.pins.erase(std::string(id));
    inner_->state.waits.erase(std::string(id));
    return inner_->state.save(inner_->config.root);
}

Result<> Updater::set_pins(std::map<std::string, Version> pins) const {
    for (const auto& [id, _] : pins)
        if (auto spec = inner_->spec(id); !spec) return std::unexpected(spec.error());
    std::lock_guard lock(inner_->mutex);
    if (inner_->state.pins != pins) {
        inner_->state.pins = std::move(pins);
        return inner_->state.save(inner_->config.root);
    }
    return {};
}

std::vector<std::pair<std::string, Result<SwapOutcome>>> Updater::apply_pending(const Activity& activity) const {
    std::vector<std::string> pending;
    {
        std::lock_guard lock(inner_->mutex);
        for (const auto& [id, _] : inner_->state.pending_swap) pending.push_back(id);
    }
    std::vector<std::pair<std::string, Result<SwapOutcome>>> out;
    for (const auto& id : pending) out.emplace_back(id, swap(id, activity));
    return out;
}

Result<SwapOutcome> Updater::swap(std::string_view id, const Activity& activity) const {
    const auto work = [&]() -> Result<SwapOutcome> {
        auto spec = inner_->spec(id);
        if (!spec) return std::unexpected(spec.error());
        Version to;
        {
            std::lock_guard lock(inner_->mutex);
            auto it = inner_->state.pending_swap.find(std::string(id));
            if (it == inner_->state.pending_swap.end()) return std::unexpected(Error::nothing_offered(id));
            to = it->second;
        }
        auto outcome = inner_->swapper.try_swap(id, to, inner_->trusted(), activity);
        if (!(outcome && outcome->type == SwapOutcome::Type::Deferred)) {
            std::lock_guard lock(inner_->mutex);
            inner_->state.pending_swap.erase(std::string(id));
            if (auto r = inner_->state.save(inner_->config.root); !r) return std::unexpected(r.error());
        }
        return outcome;
    };
    return inner_->note(id, work());
}

Result<SwapOutcome> Updater::rollback(std::string_view id, const Activity& activity) const {
    const auto work = [&]() -> Result<SwapOutcome> {
        auto spec = inner_->spec(id);
        if (!spec) return std::unexpected(spec.error());
        auto outcome = inner_->swapper.rollback(id, inner_->trusted(), activity);
        if (!outcome) return std::unexpected(outcome.error());
        if (outcome->type == SwapOutcome::Type::Swapped) {
            std::lock_guard lock(inner_->mutex);
            inner_->state.pending_swap.erase(std::string(id));
            if (auto r = inner_->state.save(inner_->config.root); !r) return std::unexpected(r.error());
        }
        return outcome;
    };
    return inner_->note(id, work());
}

Overview Updater::overview() const {
    Overview out;
    for (const auto& spec : inner_->config.products) out.products.push_back(inner_->status(spec));
    std::lock_guard lock(inner_->mutex);
    out.github_rate_limit = inner_->rate_limit;
    return out;
}

} // namespace loa::updater
