#pragma once

// Loading, editing and saving the config file.

#include <atomic>
#include <cstdint>
#include <filesystem>
#include <memory>
#include <string_view>
#include <type_traits>
#include <utility>

#include "loa/settings/app/error.hpp"
#include "loa/settings/app/schema.hpp"

namespace loa::settings::app {

// File name of the config inside the app's config directory.
inline constexpr std::string_view CONFIG_FILE_NAME = "config.toml";

// Where the config file lives inside `config_dir`; callers pick the directory.
[[nodiscard]] std::filesystem::path config_path(const std::filesystem::path& config_dir);

// The number of saves made through a store and its copies (replaces the Rust
// `watch::Receiver<u64>`): compare `value()` with a generation seen earlier.
class SaveGeneration {
  public:
    [[nodiscard]] std::uint64_t value() const noexcept { return counter_->load(std::memory_order_acquire); }

  private:
    friend class ConfigStore;
    explicit SaveGeneration(std::shared_ptr<const std::atomic<std::uint64_t>> c) : counter_(std::move(c)) {}
    std::shared_ptr<const std::atomic<std::uint64_t>> counter_;
};

// Reads and writes one config file. Copies share the save counter and the read cache.
class ConfigStore {
  public:
    explicit ConfigStore(std::filesystem::path path);

    // A store for `config_path(config_dir)`.
    [[nodiscard]] static ConfigStore in_dir(const std::filesystem::path& config_dir);

    [[nodiscard]] const std::filesystem::path& path() const noexcept { return path_; }

    // Loads the file, or the defaults when it doesn't exist yet. A file unchanged since
    // this store last read or wrote it (modification time and length) comes from memory.
    // A file of another schema version is refused (`TooNew` or `MissingMigration`).
    [[nodiscard]] Result<Config> load() const;

    // Validates `config` and replaces the file with it atomically.
    [[nodiscard]] Result<> save(const Config& config) const;

    // Loads the file, lets `change` edit it and saves it when anything changed, so
    // keys other callers wrote meanwhile survive.
    template <typename F>
    auto edit(F&& change) const -> Result<std::invoke_result_t<F, Config&>> {
        using R = std::invoke_result_t<F, Config&>;
        auto config = load();
        if (!config) return std::unexpected(std::move(config.error()));
        const Config before = *config;
        if constexpr (std::is_void_v<R>) {
            std::forward<F>(change)(*config);
            if (*config != before)
                if (auto saved = save(*config); !saved) return std::unexpected(std::move(saved.error()));
            return {};
        } else {
            R out = std::forward<F>(change)(*config);
            if (*config != before)
                if (auto saved = save(*config); !saved) return std::unexpected(std::move(saved.error()));
            return out;
        }
    }

    // Counts the saves made through this store and its copies.
    [[nodiscard]] SaveGeneration watch() const { return SaveGeneration(saves_); }

  private:
    struct Cache;
    std::filesystem::path path_;
    std::shared_ptr<std::atomic<std::uint64_t>> saves_;
    std::shared_ptr<Cache> cache_;
};

} // namespace loa::settings::app
