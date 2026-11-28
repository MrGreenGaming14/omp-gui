#pragma once

// Durable file writes, change-detecting snapshots, path confinement and well-known dirs.

#include <expected>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>

#include "loa/runtime/error.hpp"

namespace loa::runtime {
namespace json {
class Value;
}

namespace fs {

using Path = std::filesystem::path;

// Replaces `path` with `bytes` via a temp file in the same directory, fsync, rename and
// directory fsync. A symlinked `path` keeps its link; `mode` or else the existing file's
// permissions apply (a new file without `mode` gets 0600).
[[nodiscard]] std::expected<void, Error> write_atomic(const Path& path, std::string_view bytes, std::optional<unsigned> mode = {});

// `write_atomic` of `v` as pretty JSON (two-space indent, like serde_json).
[[nodiscard]] std::expected<void, Error> write_json_atomic(const Path& path, const json::Value& v);

// The file's bytes, or none when it does not exist.
[[nodiscard]] std::expected<std::optional<std::string>, Error> read_optional(const Path& path);

// Makes a rename in `dir` durable.
[[nodiscard]] std::expected<void, Error> sync_dir(const Path& dir);

// What a file held when loaded, so a save can refuse to clobber someone else's change.
class Snapshot {
  public:
    [[nodiscard]] static std::expected<Snapshot, Error> load(const Path& path);

    [[nodiscard]] const Path& path() const noexcept { return path_; }
    // The bytes seen at load or last save; none when the file was missing.
    [[nodiscard]] const std::optional<std::string>& contents() const noexcept { return original_; }

    // Writes `bytes` and returns true when the file still holds what was seen; false (no write) otherwise.
    [[nodiscard]] std::expected<bool, Error> save(std::string_view bytes);

  private:
    Snapshot(Path path, std::optional<std::string> original) : path_(std::move(path)), original_(std::move(original)) {}

    Path path_;
    std::optional<std::string> original_;
};

// `rel` under `root`, refusing `..`, absolute parts and symlinks that lead outside `root`
// (`ErrorCode::permission_denied`).
[[nodiscard]] std::expected<Path, Error> confine(const Path& root, const Path& rel);

// `$HOME` when set and non-empty, else the passwd entry's home directory.
[[nodiscard]] std::optional<Path> home();

// `realpath`.
[[nodiscard]] std::expected<Path, Error> canonicalize(const Path& path);

// `$XDG_DATA_HOME/<app>`, else `~/.local/share/<app>`, else `%APPDATA%\<app>`. Not created.
// Relative `XDG_DATA_HOME`/`APPDATA` values are ignored.
[[nodiscard]] std::optional<Path> data_dir(std::string_view app);

} // namespace fs
} // namespace loa::runtime
