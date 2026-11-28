#pragma once

// Reading the MCP server configs of Claude Code, Cursor and Codex. The files are only ever
// read; every entry ends up imported, a duplicate, or unsupported with the reason.

#include <cstdint>
#include <expected>
#include <filesystem>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/mcp/spec.hpp"

namespace loa::mcp::import {

using Path = std::filesystem::path;

enum class App : std::uint8_t { Claude, Cursor, Codex };

// A server spec, or why it can't be used.
using SpecOrReason = std::expected<ServerSpec, std::string>;

// A named server entry.
using Entry = std::pair<std::string, SpecOrReason>;

// One server entry found in a config file.
struct Found {
    App app = App::Claude;
    Path file;
    std::string name;
    SpecOrReason spec;
    friend bool operator==(const Found&, const Found&) = default;
};

// A config file of another app, read through its format's adapter.
class ImportSource {
  public:
    virtual ~ImportSource() = default;
    [[nodiscard]] virtual App app() const = 0;
    [[nodiscard]] virtual const Path& path() const = 0;
    // The `(name, entry)` pairs in the file's text.
    [[nodiscard]] virtual std::expected<std::vector<Entry>, std::string> entries(std::string_view text) const = 0;

    // The server entries in the file; a missing file has none.
    [[nodiscard]] std::expected<std::vector<Found>, std::string> read() const;
};

// `mcpServers` of a Claude (`~/.claude.json`, a project's `.mcp.json`) or Cursor JSON file.
// `project` also reads the local scope servers under `projects.<project>`.
class JsonServers final : public ImportSource {
  public:
    JsonServers(App app, Path path, std::optional<Path> project = std::nullopt)
        : app_(app), path_(std::move(path)), project_(std::move(project)) {}
    [[nodiscard]] App app() const override { return app_; }
    [[nodiscard]] const Path& path() const override { return path_; }
    [[nodiscard]] std::expected<std::vector<Entry>, std::string> entries(std::string_view text) const override;

  private:
    App app_;
    Path path_;
    std::optional<Path> project_;
};

// `[mcp_servers.<name>]` tables of `~/.codex/config.toml`.
class CodexToml final : public ImportSource {
  public:
    explicit CodexToml(Path path) : path_(std::move(path)) {}
    [[nodiscard]] App app() const override { return App::Codex; }
    [[nodiscard]] const Path& path() const override { return path_; }
    [[nodiscard]] std::expected<std::vector<Entry>, std::string> entries(std::string_view text) const override;

  private:
    Path path_;
};

// The usual config locations under a home directory and, optionally, a project directory.
[[nodiscard]] std::vector<std::unique_ptr<ImportSource>> defaults(const Path& home, const std::optional<Path>& project = std::nullopt);

namespace outcome {
struct Imported {
    ServerSpec spec;
    friend bool operator==(const Imported&, const Imported&) = default;
};
// Already registered (or imported earlier in the run) as `existing`, by name or by reaching the same server.
struct Duplicate {
    std::string existing;
    friend bool operator==(const Duplicate&, const Duplicate&) = default;
};
struct Unsupported {
    std::string reason;
    friend bool operator==(const Unsupported&, const Unsupported&) = default;
};
} // namespace outcome
using Outcome = std::variant<outcome::Imported, outcome::Duplicate, outcome::Unsupported>;

struct ImportEntry {
    App app = App::Claude;
    Path file;
    std::string name;
    Outcome outcome;
    friend bool operator==(const ImportEntry&, const ImportEntry&) = default;
};

// A file that couldn't be read or parsed; the other files still import.
struct FileError {
    Path file;
    std::string message;
    friend bool operator==(const FileError&, const FileError&) = default;
};

struct ImportReport {
    std::vector<ImportEntry> entries;
    std::vector<FileError> file_errors;

    // The specs to register.
    [[nodiscard]] std::vector<ServerSpec> imported() const;
    friend bool operator==(const ImportReport&, const ImportReport&) = default;
};

// Decides read entries (the ones the user picked) against the `existing` servers.
[[nodiscard]] ImportReport plan_found(std::span<const ServerSpec> existing, std::vector<Found> found);

// Every entry of `files`, for the import list, and the files that failed.
[[nodiscard]] std::pair<std::vector<Found>, std::vector<FileError>> scan(std::span<const std::unique_ptr<ImportSource>> files);

} // namespace loa::mcp::import
