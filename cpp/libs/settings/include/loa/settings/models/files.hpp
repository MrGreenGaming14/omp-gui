#pragma once

// Where omp keeps its `config.yml` and `models.yml`, and the port that asks omp
// for its active agent directory (port of `models/files.rs`).

#include <filesystem>
#include <utility>

#include "loa/contracts/async.hpp"
#include "loa/settings/models/error.hpp"

namespace loa::settings::models::files {

// omp's settings files in its active agent directory.
struct OmpFiles {
    std::filesystem::path agent_dir;

    // `config.yml`: roles, fallback chains, enabled models and providers.
    [[nodiscard]] std::filesystem::path config() const { return agent_dir / "config.yml"; }
    // `models.yml`: custom providers and model overrides.
    [[nodiscard]] std::filesystem::path models() const { return agent_dir / "models.yml"; }
    friend bool operator==(const OmpFiles&, const OmpFiles&) = default;
};

// Finds omp's active agent directory the way omp itself does (profiles,
// `PI_CODING_AGENT_DIR`).
class OmpFilesResolver {
  public:
    virtual ~OmpFilesResolver() = default;
    [[nodiscard]] virtual contracts::Awaitable<Result<OmpFiles>> resolve() const = 0;
};

} // namespace loa::settings::models::files
