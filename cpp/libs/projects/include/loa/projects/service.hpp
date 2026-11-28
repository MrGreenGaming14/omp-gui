#pragma once

// The projects service `loa-qt` holds: listing, creating, moving and deleting projects, the project screen,
// rebuilds, chat context and tools.

#include <chrono>
#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <memory>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

#include "loa/contracts/harness.hpp"
#include "loa/projects/digester.hpp"
#include "loa/projects/rebuild.hpp"
#include "loa/projects/sessions.hpp"
#include "loa/projects/settings.hpp"
#include "loa/projects/source.hpp"

namespace asio {
class io_context;
}

namespace loa::projects {

// `debounce`: quiet time after a turn end before a digest rebuild; `context_budget` in tokens.
struct ServiceConfig {
    std::chrono::steady_clock::duration debounce = std::chrono::seconds(10);
    std::size_t context_budget = 4'000;
    PromptLimits limits;
};

// A project as the sidebar lists it: `cwd` is its folder when set, else that of its latest session;
// `last_active` in Unix ms.
struct ProjectSummary {
    std::string name;
    std::optional<std::string> cwd;
    std::size_t sessions = 0;
    std::int64_t last_active = 0;
    friend bool operator==(const ProjectSummary&, const ProjectSummary&) = default;
};

// `folder`, unless it is the system temp folder itself (`/tmp`): then a new directory inside it, named like
// `/tmp/.tmpAbC123`, so the project's files and `.omgui` links don't mix with everything else there.
[[nodiscard]] Result<std::filesystem::path> isolated(std::filesystem::path folder);

// Cheap to copy; every copy feeds the same rebuild driver.
class ProjectService {
  public:
    // Starts the rebuild driver on `context`; it stops with the last copy.
    [[nodiscard]] static std::pair<ProjectService, std::shared_ptr<Outcomes>> start(
        LiveSessions sessions, std::shared_ptr<const SettingsStore> settings, std::shared_ptr<const Access> access,
        std::shared_ptr<const Summarizers> summarizers, ServiceConfig config, asio::io_context& context);

    // Queues a digest rebuild once `session` goes quiet (after a turn end or compaction).
    bool notify(std::string_view session) const;

    // Every project, most recently active first; also syncs each stored folder's `.omgui` links.
    [[nodiscard]] Result<std::vector<ProjectSummary>> projects() const;
    // Creates a project and returns the folder it stored, which `isolated` may have replaced.
    [[nodiscard]] Result<std::optional<std::filesystem::path>> create(std::string_view name,
                                                                      const settings::ProjectSettings& settings) const;
    // Makes `folder` (or an isolated one inside it) the project's folder and returns it.
    [[nodiscard]] Result<std::filesystem::path> set_folder(std::string_view project, std::filesystem::path folder) const;
    // Deletes `project`, keeping its sessions in no project. Returns how many sessions it had.
    [[nodiscard]] Result<std::size_t> remove(std::string_view project) const;
    // Puts `session` into `project`, or out of every project when none; a blank name counts as none.
    [[nodiscard]] Result<> move_session(std::string_view session, std::optional<std::string_view> project) const;
    // The project screen's data.
    [[nodiscard]] Result<settings::ProjectOverview> overview(std::string_view project) const;
    // Stores the settings, then the ideas notes when given. Returns the notes file as written.
    [[nodiscard]] Result<std::optional<runtime::fs::Snapshot>> save_settings(
        std::string_view project, const settings::ProjectSettings& settings,
        std::optional<std::pair<runtime::fs::Snapshot, std::string_view>> ideas) const;
    // Sets the exclude toggle (including again queues a rebuild); whether anything changed.
    [[nodiscard]] Result<bool> set_excluded(std::string_view session, bool excluded) const;
    // Markdown context for a new chat; none when empty.
    [[nodiscard]] Result<std::optional<std::string>> context(std::string_view project,
                                                             std::optional<std::string_view> session,
                                                             std::string_view query) const;
    // `project_search` and `project_read` for every new session.
    [[nodiscard]] std::pair<std::vector<contracts::HostToolSpec>, std::shared_ptr<const contracts::HostTools>> host_tools() const;

  private:
    ProjectService(LiveSessions sessions, std::shared_ptr<const SettingsStore> settings,
                   std::shared_ptr<const Access> access, RebuildSender rebuild, std::size_t context_budget)
        : sessions_(std::move(sessions)), settings_(std::move(settings)), access_(std::move(access)),
          rebuild_(std::move(rebuild)), context_budget_(context_budget) {}

    LiveSessions sessions_;
    std::shared_ptr<const SettingsStore> settings_;
    std::shared_ptr<const Access> access_;
    RebuildSender rebuild_;
    std::size_t context_budget_;
};

} // namespace loa::projects
