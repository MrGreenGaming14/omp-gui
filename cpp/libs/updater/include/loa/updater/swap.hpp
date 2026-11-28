#pragma once

// Switches a product's active version between turns, keeping the previous one for rollback.
// `<root>/versions/<product>/active.json` is replaced atomically.

#include <cstddef>
#include <optional>
#include <span>
#include <string>
#include <string_view>

#include "loa/settings/updates.hpp"
#include "loa/updater/model.hpp"
#include "loa/updater/signature.hpp"
#include "loa/updater/stage.hpp"

namespace loa::updater {

// Whether a turn runs; swaps only happen when none does.
class Activity {
  public:
    virtual ~Activity() = default;
    [[nodiscard]] virtual bool turn_running() const = 0;
};

// Swaps and rolls back versions installed under `root`.
class Swapper {
  public:
    explicit Swapper(Path root) : root(std::move(root)) {}

    [[nodiscard]] Result<std::optional<loa::settings::Active>> active(std::string_view product) const;

    // Makes `to` current unless a turn runs; `trusted` keys are required for the app and,
    // when given, for any product. Versions older than the previous one are pruned to `keep`.
    [[nodiscard]] Result<SwapOutcome> try_swap(std::string_view product, const Version& to, std::optional<std::span<const VerifyingKey>> trusted,
                                               const Activity& activity) const;
    // Makes the previous version current again, keeping the one rolled back from as previous.
    [[nodiscard]] Result<SwapOutcome> rollback(std::string_view product, std::optional<std::span<const VerifyingKey>> trusted,
                                               const Activity& activity) const;

    Path root;
    // Versions older than the previous one kept after a swap.
    std::size_t keep = 0;
    // The product whose versions must always carry a trusted signature.
    std::string app_product = "app";

  private:
    [[nodiscard]] Path active_path(std::string_view product) const;
    [[nodiscard]] Result<Staged> verified(std::string_view product, const Version& version, std::optional<std::span<const VerifyingKey>> trusted) const;
    void prune(std::string_view product, const loa::settings::Active& active) const;
};

} // namespace loa::updater
