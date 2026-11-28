#pragma once

// Products and how the updater watches them; the status the Updates screen renders lives
// in `loa::settings` (updates.hpp). JSON forms follow the Rust serde tagging and field order.

#include <expected>
#include <filesystem>
#include <optional>
#include <string>
#include <variant>
#include <vector>

#include "loa/settings/updates.hpp"
#include "loa/updater/error.hpp"
#include "loa/updater/gates.hpp"

namespace loa::updater {

using loa::settings::ProductKind;
using loa::settings::Release;

// Where a product's releases are published. JSON: `{"type":"github","owner","repo"}` or `{"type":"npm","package"}`.
struct Feed {
    enum class Type : std::uint8_t { GitHub, Npm } type = Type::GitHub;
    std::string owner;   // GitHub
    std::string repo;    // GitHub
    std::string package; // Npm

    [[nodiscard]] static Feed github(std::string owner, std::string repo) { return {Type::GitHub, std::move(owner), std::move(repo), {}}; }
    [[nodiscard]] static Feed npm(std::string package) { return {Type::Npm, {}, {}, std::move(package)}; }
    // Registry key of the feed source serving this feed.
    [[nodiscard]] std::string_view kind() const noexcept { return type == Type::GitHub ? "github" : "npm"; }
    friend bool operator==(const Feed&, const Feed&) = default;
};

// Whether prereleases (`1.2.0-beta.1`) are offered.
enum class PrereleasePolicy : std::uint8_t { Stable, Include };

// One watched product; `id` is a single path component.
struct Product {
    std::string id;
    ProductKind kind;
    Feed feed;
    PrereleasePolicy prereleases = PrereleasePolicy::Stable;
    // GitHub asset name with `{version}` placeholders; npm always installs the tarball.
    std::optional<std::string> asset;
    friend bool operator==(const Product&, const Product&) = default;
};

// A feed's last response, kept for conditional requests.
struct CachedFeed {
    std::optional<std::string> etag;
    std::vector<Release> releases;
    friend bool operator==(const CachedFeed&, const CachedFeed&) = default;
};

// Where the version in use comes from when the updater hasn't installed one.
// JSON: `{"running":"1.0.0"}` or `{"executable":"omp"}`.
struct Installed {
    enum class Type : std::uint8_t { Running, Executable } type = Type::Running;
    Version version;              // Running
    std::filesystem::path path;   // Executable: a bare name looked up on `PATH`, or a path.

    [[nodiscard]] static Installed running(Version v) { return {Type::Running, std::move(v), {}}; }
    [[nodiscard]] static Installed executable(std::filesystem::path p) { return {Type::Executable, {}, std::move(p)}; }
    friend bool operator==(const Installed&, const Installed&) = default;
};

// Which gate checks a product's updates go through. JSON: `{"type":"none"}`, `{"type":"changelog"}`,
// `{"type":"omp", <OmpGates fields>}`.
struct Gates {
    enum class Type : std::uint8_t {
        // The app relies on its signature.
        None,
        Changelog,
        Omp,
    } type = Type::None;
    OmpGates omp; // Omp

    [[nodiscard]] static Gates none() { return {Type::None, {}}; }
    [[nodiscard]] static Gates changelog() { return {Type::Changelog, {}}; }
    [[nodiscard]] static Gates omp_gates(OmpGates g) { return {Type::Omp, std::move(g)}; }
    friend bool operator==(const Gates&, const Gates&) = default;
};

// One watched product and how the updater treats it.
struct ProductSpec {
    Product product;
    std::string label;
    Installed installed;
    // Executable inside a staged payload (`{version}` replaced); none derives it.
    std::optional<std::string> executable;
    Gates gates;
    friend bool operator==(const ProductSpec&, const ProductSpec&) = default;
};

// Result of a swap or rollback attempt.
struct SwapOutcome {
    enum class Type : std::uint8_t {
        // A turn runs; nothing changed.
        Deferred,
        AlreadyActive,
        Swapped,
    } type = Type::Deferred;
    std::optional<Version> from; // Swapped
    Version to;                  // Swapped

    [[nodiscard]] static SwapOutcome deferred() { return {}; }
    [[nodiscard]] static SwapOutcome already_active() { return {Type::AlreadyActive, {}, {}}; }
    [[nodiscard]] static SwapOutcome swapped(std::optional<Version> from, Version to) { return {Type::Swapped, std::move(from), std::move(to)}; }
    friend bool operator==(const SwapOutcome&, const SwapOutcome&) = default;
};

[[nodiscard]] Json to_json(const Feed& v);
[[nodiscard]] Json to_json(PrereleasePolicy v);
[[nodiscard]] Json to_json(const Product& v);
[[nodiscard]] Json to_json(const CachedFeed& v);
[[nodiscard]] Json to_json(const Installed& v);
[[nodiscard]] Json to_json(const Gates& v);
[[nodiscard]] Json to_json(const ProductSpec& v);

// Decoders; the error is the serde-style reason.
[[nodiscard]] std::expected<Feed, std::string> feed_from_json(const Json& v);
[[nodiscard]] std::expected<Product, std::string> product_from_json(const Json& v);
[[nodiscard]] std::expected<CachedFeed, std::string> cached_feed_from_json(const Json& v);
[[nodiscard]] std::expected<Installed, std::string> installed_from_json(const Json& v);
[[nodiscard]] std::expected<Gates, std::string> gates_from_json(const Json& v);
[[nodiscard]] std::expected<ProductSpec, std::string> product_spec_from_json(const Json& v);

} // namespace loa::updater
