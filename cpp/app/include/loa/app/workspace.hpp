#pragma once

// The folder and project the app works in, recent folders and project tabs, and the git and
// folder dialog ports with their command line adapters (port of `loa-app/src/app/workspace.rs`).
// The `App` side lives in `app_workspace.cpp`.

#include <expected>
#include <filesystem>
#include <optional>
#include <set>
#include <string>
#include <vector>

#include "loa/contracts/async.hpp"
#include "loa/view/session.hpp"

namespace loa::app {

// Reads the repository a folder is in.
class GitProbe {
  public:
    virtual ~GitProbe() = default;
    // The branch checked out, or a short commit id when HEAD is detached.
    [[nodiscard]] virtual std::optional<std::string> branch(const std::filesystem::path& dir) const = 0;
    // `git diff HEAD`; `nullopt` outside a git work tree with a commit.
    [[nodiscard]] virtual contracts::Awaitable<std::optional<std::string>> diff(std::filesystem::path dir) const = 0;
};

// The desktop's folder dialog.
class FolderPicker {
  public:
    virtual ~FolderPicker() = default;
    // The folder chosen, starting in `start`; `nullopt` when cancelled.
    [[nodiscard]] virtual contracts::Awaitable<std::expected<std::optional<std::filesystem::path>, std::string>> pick(
        std::filesystem::path start) const = 0;
};

// `GitProbe` over `.git/HEAD` and the `git` command.
class GitCli final : public GitProbe {
  public:
    [[nodiscard]] std::optional<std::string> branch(const std::filesystem::path& dir) const override;
    [[nodiscard]] contracts::Awaitable<std::optional<std::string>> diff(std::filesystem::path dir) const override;
};

// `FolderPicker` over zenity (GNOME), else kdialog (KDE); on Windows the system folder dialog.
class DialogPicker final : public FolderPicker {
  public:
    [[nodiscard]] contracts::Awaitable<std::expected<std::optional<std::filesystem::path>, std::string>> pick(
        std::filesystem::path start) const override;
};

// Folders worked in this run and the project tabs.
struct Workspace {
    // Most recent first, for the folder menu.
    std::vector<std::filesystem::path> recent;
    // The projects with a tab in the project strip, in order: selected this run or of a session opened.
    std::vector<std::string> open_projects;
    // Sessions whose project was already offered a tab, so a closed tab stays closed.
    std::set<std::string, std::less<>> tabbed;
};

// How many recent folders the composer lists.
inline constexpr std::size_t RECENT_FOLDERS = 8;

// The composer's folder chip for `cwd`: home shortened to `~`, and the git branch.
[[nodiscard]] loa::view::session::composer::Workspace chip(const GitProbe& git, const std::filesystem::path& cwd);

} // namespace loa::app
