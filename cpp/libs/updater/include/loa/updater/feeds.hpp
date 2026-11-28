#pragma once

// Release feeds behind the `FeedSource` port: GitHub releases and npm packages, read with
// conditional requests so an unchanged feed costs nothing.

#include <optional>
#include <string>
#include <vector>

#include "loa/settings/updates.hpp"
#include "loa/updater/http.hpp"
#include "loa/updater/model.hpp"

namespace loa::updater::feeds {

using loa::settings::RateLimit;

// Outcome of fetching a feed.
struct FeedResponse {
    // Unchanged since the cached response.
    bool not_modified = false;
    CachedFeed feed;                      // Fresh
    std::optional<RateLimit> rate_limit;  // Fresh
};

// Lists the releases of a product from where they are published.
class FeedSource {
  public:
    virtual ~FeedSource() = default;
    [[nodiscard]] virtual Result<FeedResponse> fetch(const Product& product, const CachedFeed* cached) const = 0;
};

// The release to offer: the highest allowed version above `current`, or only the pin when one is set.
[[nodiscard]] const Release* newest(const std::vector<Release>& releases, const Version& current, PrereleasePolicy policy, const Version* pin);
// Releases in `(from, to]`, ascending.
[[nodiscard]] std::vector<const Release*> between(const std::vector<Release>& releases, const Version& from, const Version& to);
// Feed cache key covering every setting that changes what a feed returns.
[[nodiscard]] std::string cache_key(const Product& product);

// GitHub releases through the REST API (`https://api.github.com` in production).
class GitHub final : public FeedSource {
  public:
    GitHub(Http client, std::string api, std::optional<std::string> token = {})
        : client(std::move(client)), api(std::move(api)), token(std::move(token)) {}
    // Leaves out drafts, non-semver tags and, for stable products, prereleases.
    [[nodiscard]] Result<FeedResponse> fetch(const Product& product, const CachedFeed* cached) const override;

    Http client;
    std::string api;
    // Raises GitHub's rate limit when set.
    std::optional<std::string> token;
};

// Package versions from an npm registry (`https://registry.npmjs.org` in production).
class Npm final : public FeedSource {
  public:
    Npm(Http client, std::string registry) : client(std::move(client)), registry(std::move(registry)) {}
    // Leaves out deprecated versions and, for stable products, everything above the `latest` dist-tag.
    [[nodiscard]] Result<FeedResponse> fetch(const Product& product, const CachedFeed* cached) const override;

    Http client;
    std::string registry;
};

} // namespace loa::updater::feeds
