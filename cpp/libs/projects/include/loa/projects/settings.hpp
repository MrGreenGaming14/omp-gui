#pragma once

// Project settings, each project's ideas notes and the per session exclude toggle.

#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

#include "loa/core/dom.hpp"
#include "loa/core/patch.hpp"
#include "loa/projects/error.hpp"
#include "loa/runtime/json.hpp"
#include "loa/settings/projects.hpp"

namespace loa::projects {

// Stores project settings and ideas notes; a project without any gets defaults and empty notes.
class SettingsStore {
  public:
    virtual ~SettingsStore() = default;
    [[nodiscard]] virtual Result<settings::ProjectSettings> load(std::string_view project) const = 0;
    [[nodiscard]] virtual Result<> save(std::string_view project, const settings::ProjectSettings& settings) const = 0;
    // The projects that have settings, sorted by name.
    [[nodiscard]] virtual Result<std::vector<std::string>> names() const = 0;
    // Forgets the project's settings and ideas notes; nothing happens when it has none.
    [[nodiscard]] virtual Result<> remove(std::string_view project) const = 0;
    // The project's ideas notes as the user wrote them, empty when it has none, with the file as seen.
    [[nodiscard]] virtual Result<settings::Ideas> load_ideas(std::string_view project) const = 0;
    // Replaces the project's ideas notes with `text`, byte for byte, unless the file changed since `seen`
    // was loaded: then ChangedOnDisk and nothing is written. Returns the file as written.
    [[nodiscard]] virtual Result<runtime::fs::Snapshot> save_ideas(std::string_view project, runtime::fs::Snapshot seen,
                                                                  std::string_view text) const = 0;
};

// One JSON file per project in `dir`, written atomically, named by the project percent encoded. The
// project's ideas notes are the Markdown file of the same name ending in `.ideas.md`.
class FileSettingsStore final : public SettingsStore {
  public:
    explicit FileSettingsStore(std::filesystem::path dir) : dir_(std::move(dir)) {}

    [[nodiscard]] const std::filesystem::path& dir() const noexcept { return dir_; }
    [[nodiscard]] std::filesystem::path path(std::string_view project) const;
    [[nodiscard]] std::filesystem::path ideas_path(std::string_view project) const;

    [[nodiscard]] Result<settings::ProjectSettings> load(std::string_view project) const override;
    [[nodiscard]] Result<> save(std::string_view project, const settings::ProjectSettings& settings) const override;
    [[nodiscard]] Result<std::vector<std::string>> names() const override;
    [[nodiscard]] Result<> remove(std::string_view project) const override;
    [[nodiscard]] Result<settings::Ideas> load_ideas(std::string_view project) const override;
    [[nodiscard]] Result<runtime::fs::Snapshot> save_ideas(std::string_view project, runtime::fs::Snapshot seen,
                                                          std::string_view text) const override;

  private:
    std::filesystem::path dir_;
};

// The file stem for `project`: every byte except ASCII alphanumerics, `-` and `_` as `%XX` (uppercase hex).
[[nodiscard]] std::string encode_name(std::string_view project);
// Undoes `encode_name`; none for a malformed escape or bytes that aren't UTF-8.
[[nodiscard]] std::optional<std::string> decode_name(std::string_view stem);

// serde_json of `ProjectSettings` (`skip_serializing_if` fields left out).
[[nodiscard]] runtime::json::Value settings_to_json(const settings::ProjectSettings& settings);
[[nodiscard]] std::expected<settings::ProjectSettings, std::string> settings_from_json(const runtime::json::Value& json);

[[nodiscard]] bool is_excluded(const core::Dom& dom);
// The draft that sets the exclude toggle, none when it already has that value.
[[nodiscard]] std::optional<core::Draft> exclude_draft(const core::Dom& dom, bool excluded);

} // namespace loa::projects
