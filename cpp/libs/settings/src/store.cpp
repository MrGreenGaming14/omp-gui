#include "loa/settings/app/store.hpp"

#include <fstream>
#include <mutex>
#include <optional>
#include <sstream>
#include <system_error>

#include "loa/runtime/fs.hpp"
#include "loa/settings/app/codec.hpp"

namespace loa::settings::app {
namespace fs = std::filesystem;

namespace {

// Modification time and length of the config file: the read cache's key.
struct Stamp {
    fs::file_time_type modified;
    std::uintmax_t length = 0;
    friend bool operator==(const Stamp&, const Stamp&) = default;
};

// None when the file is missing or its metadata can't be read.
std::optional<Stamp> stamp(const fs::path& path) {
    std::error_code ec;
    auto modified = fs::last_write_time(path, ec);
    if (ec) return std::nullopt;
    auto length = fs::file_size(path, ec);
    if (ec) return std::nullopt;
    return Stamp{modified, length};
}

} // namespace

struct ConfigStore::Cache {
    std::mutex mutex;
    // The file as last read or written, with its stamp.
    std::optional<std::pair<Stamp, Config>> entry;
};

fs::path config_path(const fs::path& config_dir) { return config_dir / CONFIG_FILE_NAME; }

ConfigStore::ConfigStore(fs::path path)
    : path_(std::move(path)), saves_(std::make_shared<std::atomic<std::uint64_t>>(0)), cache_(std::make_shared<Cache>()) {}

ConfigStore ConfigStore::in_dir(const fs::path& config_dir) { return ConfigStore(config_path(config_dir)); }

Result<Config> ConfigStore::load() const {
    auto on_disk = stamp(path_);
    {
        std::lock_guard lock(cache_->mutex);
        if (cache_->entry && on_disk && cache_->entry->first == *on_disk) return cache_->entry->second;
    }
    auto text = loa::runtime::fs::read_optional(path_);
    if (!text) {
        ConfigError e;
        e.kind = ConfigError::Kind::Read;
        e.path = path_;
        e.message = text.error().message();
        return std::unexpected(std::move(e));
    }
    if (!*text) {
        std::lock_guard lock(cache_->mutex);
        cache_->entry.reset();
        return Config{};
    }
    auto doc = parse(**text);
    if (!doc) return std::unexpected(std::move(doc.error()));
    auto version = version_of(*doc);
    if (!version) return std::unexpected(std::move(version.error()));
    if (*version > CURRENT_VERSION) {
        ConfigError e;
        e.kind = ConfigError::Kind::TooNew;
        e.found = *version;
        e.supported = CURRENT_VERSION;
        return std::unexpected(std::move(e));
    }
    if (*version < CURRENT_VERSION) {
        ConfigError e;
        e.kind = ConfigError::Kind::MissingMigration;
        e.from = *version;
        return std::unexpected(std::move(e));
    }
    auto config = decode(std::move(*doc));
    if (!config) return config;
    std::lock_guard lock(cache_->mutex);
    if (on_disk) cache_->entry.emplace(*on_disk, *config);
    else cache_->entry.reset();
    return config;
}

Result<> ConfigStore::save(const Config& config) const {
    if (auto valid = config.validate(); !valid) return valid;
    auto text = encode(config);
    if (!text) return std::unexpected(std::move(text.error()));
    std::lock_guard lock(cache_->mutex);
    cache_->entry.reset();
    if (auto written = loa::runtime::fs::write_atomic(path_, *text); !written) {
        ConfigError e;
        e.kind = ConfigError::Kind::Write;
        e.path = path_;
        e.message = written.error().message();
        return std::unexpected(std::move(e));
    }
    if (auto at = stamp(path_)) cache_->entry.emplace(*at, config);
    saves_->fetch_add(1, std::memory_order_acq_rel);
    return {};
}

} // namespace loa::settings::app
