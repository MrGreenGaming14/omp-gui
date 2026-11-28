#pragma once

// Installs a release beside the active version under `<root>/versions/<product>/<version>/`
// (`payload/`, `artifact/`, `staged.json`). Work happens in a temporary directory renamed into
// place with `staged.json` written last, so a failed staging leaves nothing behind.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>

#include "loa/settings/updates.hpp"
#include "loa/updater/error.hpp"
#include "loa/updater/http.hpp"

namespace loa::updater {

inline constexpr std::string_view MANIFEST_FILE = "staged.json";

// Written to `staged.json` once a version is fully staged.
struct StagedManifest {
    std::string product;
    Version version;
    // File name inside `artifact/`.
    std::string artifact_name;
    std::string sha256;
    std::uint64_t size = 0;
    std::optional<std::string> signature_name;
    friend bool operator==(const StagedManifest&, const StagedManifest&) = default;
};

// A fully staged version.
struct Staged {
    Path dir;
    StagedManifest manifest;

    [[nodiscard]] Path payload() const { return dir / "payload"; }
    [[nodiscard]] Path artifact() const { return dir / "artifact" / manifest.artifact_name; }
    [[nodiscard]] std::optional<Path> signature() const;
    friend bool operator==(const Staged&, const Staged&) = default;
};

namespace stage {

// Directory holding every version of `product`.
[[nodiscard]] Path product_dir(const Path& root, std::string_view product);
[[nodiscard]] Path version_dir(const Path& root, std::string_view product, const Version& version);
// The staged version of `product` at `version`, when fully staged.
[[nodiscard]] Result<std::optional<Staged>> staged(const Path& root, std::string_view product, const Version& version);

} // namespace stage

// Downloads, verifies and unpacks releases under an updater root.
class Stager {
  public:
    Stager(Http client, Path root) : client_(std::move(client)), root_(std::move(root)) {}

    // Stages `release`; returns the existing staging when that version is already staged.
    [[nodiscard]] Result<Staged> stage(const loa::settings::Release& release) const;

  private:
    Http client_;
    Path root_;
};

[[nodiscard]] loa::runtime::json::Value to_json(const StagedManifest& v);
[[nodiscard]] std::expected<StagedManifest, std::string> staged_manifest_from_json(const loa::runtime::json::Value& v);

} // namespace loa::updater
