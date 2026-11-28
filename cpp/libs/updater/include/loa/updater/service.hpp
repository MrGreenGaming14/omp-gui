#pragma once

// The updater as one service the Updates screen drives: `detect` finds what is installed,
// `check` reads a feed, `stage` installs the offer side by side, `gate` records swap or hold,
// `choose` answers a hold, `apply_pending` swaps between turns and `rollback` goes back one.
// Every call blocks; run them off the UI thread. Copies share state.

#include <atomic>
#include <chrono>
#include <cstddef>
#include <map>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/settings/updates.hpp"
#include "loa/updater/gates.hpp"
#include "loa/updater/model.hpp"
#include "loa/updater/signature.hpp"
#include "loa/updater/swap.hpp"

namespace loa::updater {

// Settings of the updater service.
struct UpdaterConfig {
    // Directory holding versions, state and feed cache.
    Path root;
    std::vector<ProductSpec> products;
    std::string github_api = "https://api.github.com";
    std::string npm_registry = "https://registry.npmjs.org";
    // Raises GitHub's rate limit when set.
    std::optional<std::string> github_token;
    // Keys app updates must be signed with.
    std::vector<VerifyingKey> trusted_keys;
    // The gate adapters (omp-backed in the app); without them only signature-free checks run.
    std::shared_ptr<GatePorts> ports;
    // Product whose executable reviews changelogs, and the `--model` it reviews with.
    std::string reviewer = "omp";
    std::string review_model = "@smol";
    std::chrono::milliseconds probe_timeout = std::chrono::seconds(60);
    std::chrono::milliseconds review_timeout = std::chrono::seconds(180);

    // Production feeds, the smol role for reviews, no trusted keys, no gate adapters.
    UpdaterConfig(Path root, std::vector<ProductSpec> products) : root(std::move(root)), products(std::move(products)) {}
};

// Release asset suffix of this platform as omp names its binaries (`linux-x64`, `darwin-arm64`).
[[nodiscard]] std::string platform();

// The products the app watches: itself, omp and the harnesses its adapters drive.
[[nodiscard]] std::vector<ProductSpec> default_products(const Version& app_version);

// Counts running turns; swaps wait until none runs.
class TurnGate final : public Activity {
  public:
    void begin() noexcept { ++running_; }
    void end() noexcept;
    // Sets the number of running turns, e.g. recounted from the sessions.
    void set(std::size_t running) noexcept { running_ = running; }
    [[nodiscard]] bool turn_running() const override { return running_.load() > 0; }

  private:
    std::atomic<std::size_t> running_{0};
};

// Runs `<executable> --version` and reads the first semantic version it prints.
[[nodiscard]] std::expected<Version, std::string> executable_version(const Path& executable, std::chrono::milliseconds timeout);

class Updater {
  public:
    // Opens the updater under `config.root`, loading state and feed cache. Nothing is fetched.
    [[nodiscard]] static Result<Updater> open(UpdaterConfig config);

    [[nodiscard]] const UpdaterConfig& config() const;

    // Finds every product's executable and asks it for its version.
    void detect() const;
    // The executable new sessions of `id` start: the active staged version, else the system one.
    [[nodiscard]] std::optional<Path> executable(std::string_view id) const;
    // Reads `id`'s feed conditionally and returns the release to offer, if any.
    [[nodiscard]] Result<std::optional<Release>> check(std::string_view id) const;
    // Checks every product's feed.
    [[nodiscard]] std::vector<std::pair<std::string, Result<std::optional<Release>>>> check_all() const;
    // Downloads, verifies and unpacks the offer; app releases have their signature checked now.
    [[nodiscard]] Result<Version> stage(std::string_view id) const;
    // Checks the signature of the staged offer of `id`.
    [[nodiscard]] Result<loa::settings::SignatureStatus> verify(std::string_view id) const;
    // Gates the staged offer against the version in use and records swap or hold.
    [[nodiscard]] Result<GateReport> gate(std::string_view id) const;
    // Applies the user's answer to `id`'s hold; returns the version now pending swap.
    [[nodiscard]] Result<std::optional<Version>> choose(std::string_view id, loa::settings::HoldChoice choice) const;
    // Removes `id`'s pin and wait.
    [[nodiscard]] Result<> unpin(std::string_view id) const;
    // Replaces every pin, e.g. with the config file's. Unknown products are refused.
    [[nodiscard]] Result<> set_pins(std::map<std::string, Version> pins) const;
    // Swaps in every cleared version unless a turn runs.
    [[nodiscard]] std::vector<std::pair<std::string, Result<SwapOutcome>>> apply_pending(const Activity& activity) const;
    // Swaps in `id`'s pending version unless a turn runs; a failed swap is dropped from pending.
    [[nodiscard]] Result<SwapOutcome> swap(std::string_view id, const Activity& activity) const;
    // Makes the previous version of `id` current again, unless a turn runs.
    [[nodiscard]] Result<SwapOutcome> rollback(std::string_view id, const Activity& activity) const;
    // A snapshot of every product; runs nothing.
    [[nodiscard]] loa::settings::Overview overview() const;

    struct Inner;

  private:
    explicit Updater(std::shared_ptr<Inner> inner) : inner_(std::move(inner)) {}
    std::shared_ptr<Inner> inner_;
};

} // namespace loa::updater
