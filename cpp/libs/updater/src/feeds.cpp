#include "loa/updater/feeds.hpp"

#include <algorithm>
#include <map>

#include "loa/runtime/time.hpp"
#include "loa/updater/signature.hpp"
#include "util.hpp"

namespace loa::updater::feeds {

using namespace detail;
using loa::settings::ArchiveFormat;
using loa::settings::Artifact;
using loa::settings::Checksum;

const Release* newest(const std::vector<Release>& releases, const Version& current, PrereleasePolicy policy, const Version* pin) {
    if (pin) {
        if (*pin == current) return nullptr;
        for (const auto& r : releases)
            if (r.version == *pin) return &r;
        return nullptr;
    }
    const Release* best = nullptr;
    for (const auto& r : releases) {
        if (!(r.version > current) || (policy != PrereleasePolicy::Include && !r.version.pre.empty())) continue;
        // `max_by` keeps the last of equal maxima.
        if (!best || !(r.version < best->version)) best = &r;
    }
    return best;
}

std::vector<const Release*> between(const std::vector<Release>& releases, const Version& from, const Version& to) {
    std::vector<const Release*> out;
    for (const auto& r : releases)
        if (r.version > from && r.version <= to) out.push_back(&r);
    std::ranges::stable_sort(out, [](const Release* a, const Release* b) { return a->version < b->version; });
    return out;
}

std::string cache_key(const Product& product) {
    const std::string policy = product.prereleases == PrereleasePolicy::Stable ? "stable" : "include";
    const std::string source = product.feed.type == Feed::Type::GitHub ? "github:" + product.feed.owner + "/" + product.feed.repo
                                                                       : "npm:" + product.feed.package;
    return product.id + "|" + source + "|" + policy + "|" + product.asset.value_or("");
}

namespace {

// A listing with its ETag and rate limit, or none when the feed answered 304.
struct Listing {
    Json value;
    std::optional<std::string> etag;
    std::optional<RateLimit> rate_limit;
};

// A 403 or 429 with an exhausted limit or a `retry-after` is rate limited; anything else is a status error.
Error reject(const std::string& url, long status, const HttpHeaders& headers) {
    if (status == 403 || status == 429) {
        const auto remaining = headers.number("x-ratelimit-remaining");
        const auto reset = headers.number("x-ratelimit-reset");
        const auto retry_after = headers.number("retry-after");
        const auto now = loa::runtime::time::now_secs();
        const auto saturating = [](std::uint64_t a, std::uint64_t b) { return a + b < a ? ~std::uint64_t{0} : a + b; };
        std::optional<std::uint64_t> retry_at;
        if (remaining == 0u && reset) retry_at = *reset;
        else if (retry_after) retry_at = saturating(now, *retry_after);
        // Exhausted without a reset time: GitHub asks clients to wait at least a minute.
        else if (remaining == 0u) retry_at = saturating(now, 60);
        if (retry_at) return Error::rate_limited(*retry_at);
    }
    return Error::status(url, status);
}

// Sends a GET with the cached ETag and reads a JSON answer.
Result<std::optional<Listing>> conditional(const Http& client, const std::string& url, Http::RequestHeaders headers, const CachedFeed* cached) {
    if (cached && cached->etag) headers.emplace_back("If-None-Match", *cached->etag);
    auto response = client.get(url, headers);
    if (!response) return std::unexpected(response.error());
    if (response->status == 304) return std::optional<Listing>{};
    if (response->status < 200 || response->status >= 300) return std::unexpected(reject(url, response->status, response->headers));
    auto value = loa::runtime::json::parse(response->body);
    if (!value) return std::unexpected(Error::feed(url + " returned malformed JSON: " + value.error().message()));
    std::optional<RateLimit> rate_limit;
    const auto remaining = response->headers.number("x-ratelimit-remaining");
    const auto reset = response->headers.number("x-ratelimit-reset");
    if (remaining && reset) rate_limit = RateLimit{*remaining, *reset};
    return std::optional<Listing>{Listing{std::move(*value), response->headers.get("etag"), rate_limit}};
}

Error wrong_feed(const Product& product) { return Error::feed(product.id + " is not published on this feed"); }

std::string trim_end_slashes(std::string s) {
    while (!s.empty() && s.back() == '/') s.pop_back();
    return s;
}

std::string replace_all(std::string s, std::string_view from, std::string_view to) {
    for (std::size_t at = s.find(from); at != std::string::npos; at = s.find(from, at + to.size())) s.replace(at, from.size(), to);
    return s;
}

std::optional<std::string> sha256_hex(std::string_view s) {
    if (s.size() != 64 || !std::ranges::all_of(s, [](unsigned char c) { return std::isxdigit(c); })) return std::nullopt;
    std::string out(s);
    std::ranges::transform(out, out.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return out;
}

ArchiveFormat format_of(std::string_view name) {
    std::string lower(name);
    std::ranges::transform(lower, lower.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    if (lower.ends_with(".tar.gz") || lower.ends_with(".tgz")) return ArchiveFormat::TarGz;
    if (lower.ends_with(".zip")) return ArchiveFormat::Zip;
    return ArchiveFormat::Raw;
}

// Strict standard base64 with padding.
std::optional<std::vector<std::uint8_t>> base64_decode(std::string_view text) {
    const auto value = [](char c) -> int {
        if (c >= 'A' && c <= 'Z') return c - 'A';
        if (c >= 'a' && c <= 'z') return c - 'a' + 26;
        if (c >= '0' && c <= '9') return c - '0' + 52;
        if (c == '+') return 62;
        if (c == '/') return 63;
        return -1;
    };
    if (text.size() % 4) return std::nullopt;
    std::vector<std::uint8_t> out;
    for (std::size_t i = 0; i < text.size(); i += 4) {
        int v[4];
        int pad = 0;
        for (int j = 0; j < 4; ++j) {
            const char c = text[i + static_cast<std::size_t>(j)];
            if (c == '=' && i + 4 == text.size() && j >= 2) {
                v[j] = 0;
                ++pad;
            } else if (pad || (v[j] = value(c)) < 0) {
                return std::nullopt;
            }
        }
        const std::uint32_t n = static_cast<std::uint32_t>(v[0] << 18 | v[1] << 12 | v[2] << 6 | v[3]);
        out.push_back(static_cast<std::uint8_t>(n >> 16));
        if (pad < 2) out.push_back(static_cast<std::uint8_t>(n >> 8));
        if (pad < 1) out.push_back(static_cast<std::uint8_t>(n));
    }
    return out;
}

// The SHA-512 of an SRI `integrity` string (`sha512-<base64>`), as hex.
std::optional<std::string> sha512_hex(std::string_view integrity) {
    std::size_t start = 0;
    while (start < integrity.size()) {
        while (start < integrity.size() && std::isspace(static_cast<unsigned char>(integrity[start]))) ++start;
        auto end = start;
        while (end < integrity.size() && !std::isspace(static_cast<unsigned char>(integrity[end]))) ++end;
        auto part = integrity.substr(start, end - start);
        start = end;
        if (!part.starts_with("sha512-")) continue;
        part.remove_prefix(7);
        part = part.substr(0, part.find('?'));
        auto bytes = base64_decode(part);
        if (bytes && bytes->size() == 64) return to_hex(*bytes);
    }
    return std::nullopt;
}

struct GhAsset {
    std::string name;
    std::string url;
    std::optional<std::uint64_t> size;
    std::optional<std::string> digest;
};

struct GhRelease {
    std::string tag_name;
    bool draft = false;
    bool prerelease = false;
    std::optional<std::string> body;
    std::optional<std::string> published_at;
    std::vector<GhAsset> assets;
};

std::vector<GhRelease> gh_releases(const Json& v) {
    std::vector<GhRelease> out;
    for (const auto& item : array(v)) {
        const auto& o = object(item, "struct GhRelease");
        GhRelease r;
        r.tag_name = string(required(o, "tag_name"));
        if (const auto* d = field(o, "draft")) r.draft = boolean(*d);
        if (const auto* p = field(o, "prerelease")) r.prerelease = boolean(*p);
        r.body = opt_string(field(o, "body"));
        r.published_at = opt_string(field(o, "published_at"));
        if (const auto* assets = field(o, "assets")) {
            for (const auto& a : array(*assets)) {
                const auto& ao = object(a, "struct GhAsset");
                GhAsset asset{string(required(ao, "name")), string(required(ao, "browser_download_url")), std::nullopt, opt_string(field(ao, "digest"))};
                if (const auto* s = field(ao, "size"); s && !s->is_null()) asset.size = u64(*s);
                r.assets.push_back(std::move(asset));
            }
        }
        out.push_back(std::move(r));
    }
    return out;
}

} // namespace

Result<FeedResponse> GitHub::fetch(const Product& product, const CachedFeed* cached) const {
    if (product.feed.type != Feed::Type::GitHub) return std::unexpected(wrong_feed(product));
    const auto url = trim_end_slashes(api) + "/repos/" + product.feed.owner + "/" + product.feed.repo + "/releases?per_page=100";
    Http::RequestHeaders headers{{"Accept", "application/vnd.github+json"}, {"X-GitHub-Api-Version", "2022-11-28"}};
    if (token) headers.emplace_back("Authorization", "Bearer " + *token);
    auto listing = conditional(client, url, std::move(headers), cached);
    if (!listing) return std::unexpected(listing.error());
    if (!*listing) return FeedResponse{true, {}, std::nullopt};
    auto listed = decoding([&] { return gh_releases((*listing)->value); });
    if (!listed) return std::unexpected(Error::feed(url + " returned malformed JSON: " + listed.error()));

    // The asset `name` with its checksum (asset digest or `.sha256` sidecar) and `.sig` URL.
    const auto artifact = [&](const std::string& name, const std::vector<GhAsset>& assets) -> Result<std::optional<Artifact>> {
        const auto find = [&](const std::string& n) -> const GhAsset* {
            for (const auto& a : assets)
                if (a.name == n) return &a;
            return nullptr;
        };
        const auto* asset = find(name);
        if (!asset) return std::optional<Artifact>{};
        std::optional<std::string> checksum;
        if (asset->digest && asset->digest->starts_with("sha256:")) checksum = sha256_hex(std::string_view(*asset->digest).substr(7));
        if (!checksum) {
            if (const auto* sidecar = find(name + ".sha256")) {
                // The digest in a `.sha256` sidecar (`<hex>` or `<hex>  <file>`); none leaves the artifact unverifiable.
                auto response = client.get(sidecar->url);
                if (!response) return std::unexpected(response.error());
                if (response->status < 200 || response->status >= 300)
                    return std::unexpected(reject(sidecar->url, response->status, response->headers));
                std::string_view text = response->body;
                const auto start = text.find_first_not_of(" \t\r\n\f\v");
                if (start != std::string_view::npos) {
                    text.remove_prefix(start);
                    checksum = sha256_hex(text.substr(0, text.find_first_of(" \t\r\n\f\v")));
                }
            }
        }
        Artifact out;
        out.name = name;
        out.url = asset->url;
        out.size = asset->size;
        if (checksum) out.checksum = Checksum{Checksum::Algorithm::Sha256, *checksum};
        out.format = format_of(name);
        if (const auto* sig = find(name + ".sig")) out.signature_url = sig->url;
        return std::optional<Artifact>{std::move(out)};
    };

    FeedResponse response{false, CachedFeed{(*listing)->etag, {}}, (*listing)->rate_limit};
    for (auto& release : *listed) {
        std::string_view tag = release.tag_name;
        if (tag.starts_with('v')) tag.remove_prefix(1);
        const bool stable_only = product.prereleases == PrereleasePolicy::Stable;
        auto version = Version::parse(tag);
        if (!version || release.draft || (release.prerelease && stable_only)) continue;
        std::optional<Artifact> art;
        if (product.asset) {
            auto found = artifact(replace_all(*product.asset, "{version}", version->to_string()), release.assets);
            if (!found) return std::unexpected(found.error());
            art = std::move(*found);
        }
        response.feed.releases.push_back(Release{product.id, std::move(*version), release.body.value_or(""), release.published_at, std::move(art)});
    }
    return response;
}

Result<FeedResponse> Npm::fetch(const Product& product, const CachedFeed* cached) const {
    if (product.feed.type != Feed::Type::Npm) return std::unexpected(wrong_feed(product));
    const auto url = trim_end_slashes(registry) + "/" + replace_all(product.feed.package, "/", "%2F");
    auto listing = conditional(client, url, {{"Accept", "application/json"}}, cached);
    if (!listing) return std::unexpected(listing.error());
    if (!*listing) return FeedResponse{true, {}, std::nullopt};

    struct Entry {
        std::string tarball;
        std::optional<std::string> integrity;
        bool deprecated = false;
    };
    struct Packument {
        std::map<std::string, std::string> dist_tags;
        std::map<std::string, Entry> versions;
        std::map<std::string, std::optional<std::string>> time;
    };
    auto packument = decoding([&] {
        const auto& o = object((*listing)->value, "struct Packument");
        Packument p;
        if (const auto* tags = field(o, "dist-tags"))
            for (const auto& [k, v] : object(*tags, "a map")) p.dist_tags.insert_or_assign(k, string(v));
        if (const auto* versions = field(o, "versions")) {
            for (const auto& [k, v] : object(*versions, "a map")) {
                const auto& vo = object(v, "struct NpmVersion");
                const auto& dist = object(required(vo, "dist"), "struct Dist");
                const auto* deprecated = field(vo, "deprecated");
                p.versions.insert_or_assign(k, Entry{string(required(dist, "tarball")), opt_string(field(dist, "integrity")),
                                                     deprecated && !deprecated->is_null()});
            }
        }
        if (const auto* time = field(o, "time"))
            for (const auto& [k, v] : object(*time, "a map")) p.time.insert_or_assign(k, v.as_string() ? std::optional(*v.as_string()) : std::nullopt);
        return p;
    });
    if (!packument) return std::unexpected(Error::feed(url + " returned malformed JSON: " + packument.error()));

    std::optional<Version> cap;
    if (const auto latest = packument->dist_tags.find("latest");
        latest != packument->dist_tags.end() && product.prereleases == PrereleasePolicy::Stable) {
        auto parsed = Version::parse(latest->second);
        if (!parsed) return std::unexpected(Error::feed("invalid version " + debug_str(latest->second) + ": " + parsed.error().message()));
        cap = std::move(*parsed);
    }
    FeedResponse response{false, CachedFeed{(*listing)->etag, {}}, std::nullopt};
    for (const auto& [key, entry] : packument->versions) {
        auto version = Version::parse(key);
        if (!version || entry.deprecated || (cap && *version > *cap)) continue;
        const auto slash = entry.tarball.rfind('/');
        std::string name = slash == std::string::npos ? entry.tarball : entry.tarball.substr(slash + 1);
        if (name.empty()) name = "package.tgz";
        Artifact artifact;
        artifact.name = std::move(name);
        artifact.url = entry.tarball;
        if (entry.integrity)
            if (auto hex = sha512_hex(*entry.integrity)) artifact.checksum = Checksum{Checksum::Algorithm::Sha512, std::move(*hex)};
        artifact.format = ArchiveFormat::NpmTarball;
        std::optional<std::string> published_at;
        if (const auto t = packument->time.find(key); t != packument->time.end()) published_at = t->second;
        response.feed.releases.push_back(Release{product.id, std::move(*version), "", std::move(published_at), std::move(artifact)});
    }
    return response;
}

} // namespace loa::updater::feeds
