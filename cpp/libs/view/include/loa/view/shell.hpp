#pragma once

// The app frame: sidebar sessions, nodes and usage, the right panel's tabs, and
// what the frame asks the app to do.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <variant>

#include "loa/contracts/session.hpp"
#include "loa/view/theme.hpp"

namespace loa::view::shell {

using loa::contracts::SessionSummary;

// The sidebar footer's usage meter: the used share (0 to 1) of the tightest limit and its window, e.g. "7d".
struct UsageMeter {
    float fraction = 0;
    std::string label;

    friend bool operator==(const UsageMeter&, const UsageMeter&) = default;
};

// Which machine's sessions the sidebar lists: every node (`node` empty) or one by key.
struct NodeFilter {
    std::optional<std::string> node;

    [[nodiscard]] bool matches(std::string_view key) const noexcept;

    friend bool operator==(const NodeFilter&, const NodeFilter&) = default;
};

// A machine the sidebar lists sessions of.
struct NodeStatus {
    // Public key, as written in `<session node>`.
    std::string key;
    std::string name;
    bool online = false;
    // This is the node the app runs on.
    bool local = false;

    friend bool operator==(const NodeStatus&, const NodeStatus&) = default;
};

// One tab of the right panel; the app draws the body of the active one by `id`.
struct PanelTab {
    std::string id;
    std::string title;
    std::optional<theme::Icon> icon;

    friend bool operator==(const PanelTab&, const PanelTab&) = default;
};

// Navigation, pin and project changes from the sidebar.
namespace sidebar_intent {
struct NewSession {
    friend bool operator==(const NewSession&, const NewSession&) = default;
};
struct OpenSession {
    std::string id;
    friend bool operator==(const OpenSession&, const OpenSession&) = default;
};
struct OpenProject {
    std::string project;
    friend bool operator==(const OpenProject&, const OpenProject&) = default;
};
struct OpenPeers {
    friend bool operator==(const OpenPeers&, const OpenPeers&) = default;
};
struct OpenTools {
    friend bool operator==(const OpenTools&, const OpenTools&) = default;
};
struct OpenUpdates {
    friend bool operator==(const OpenUpdates&, const OpenUpdates&) = default;
};
struct OpenUsage {
    friend bool operator==(const OpenUsage&, const OpenUsage&) = default;
};
struct OpenConfigs {
    friend bool operator==(const OpenConfigs&, const OpenConfigs&) = default;
};
struct OpenSettings {
    friend bool operator==(const OpenSettings&, const OpenSettings&) = default;
};
struct SetPinned {
    std::string id;
    bool pinned = false;
    friend bool operator==(const SetPinned&, const SetPinned&) = default;
};
struct SetProjectPinned {
    std::string project;
    bool pinned = false;
    friend bool operator==(const SetProjectPinned&, const SetProjectPinned&) = default;
};
struct DeleteSession {
    std::string id;
    friend bool operator==(const DeleteSession&, const DeleteSession&) = default;
};
// Open the New project screen: the `+` in the Projects header.
struct NewProject {
    friend bool operator==(const NewProject&, const NewProject&) = default;
};
// Start a new session in this project, in its folder: the `+` on its row.
struct NewSessionIn {
    std::string project;
    friend bool operator==(const NewSessionIn&, const NewSessionIn&) = default;
};
// Ask to delete this project; its sessions are kept.
struct DeleteProject {
    std::string project;
    friend bool operator==(const DeleteProject&, const DeleteProject&) = default;
};
// Move a session to a project, or out of every project with `std::nullopt`.
struct MoveSession {
    std::string id;
    std::optional<std::string> project;
    friend bool operator==(const MoveSession&, const MoveSession&) = default;
};
} // namespace sidebar_intent

using SidebarIntent = std::variant<sidebar_intent::NewSession, sidebar_intent::OpenSession, sidebar_intent::OpenProject,
                                   sidebar_intent::OpenPeers, sidebar_intent::OpenTools, sidebar_intent::OpenUpdates,
                                   sidebar_intent::OpenUsage, sidebar_intent::OpenConfigs, sidebar_intent::OpenSettings,
                                   sidebar_intent::SetPinned, sidebar_intent::SetProjectPinned,
                                   sidebar_intent::DeleteSession, sidebar_intent::NewProject,
                                   sidebar_intent::NewSessionIn, sidebar_intent::DeleteProject,
                                   sidebar_intent::MoveSession>;

// What the frame asks the app to do.
namespace intent {
// Navigation from the sidebar, the top bar crumb or the project tabs.
struct Sidebar {
    SidebarIntent intent;
    friend bool operator==(const Sidebar&, const Sidebar&) = default;
};
// Drop this project's tab; its sessions stay open.
struct CloseProjectTab {
    std::string project;
    friend bool operator==(const CloseProjectTab&, const CloseProjectTab&) = default;
};
struct DetachTab {
    std::string tab;
    friend bool operator==(const DetachTab&, const DetachTab&) = default;
};
struct ShowDetached {
    std::string tab;
    friend bool operator==(const ShowDetached&, const ShowDetached&) = default;
};
struct ReattachTab {
    std::string tab;
    friend bool operator==(const ReattachTab&, const ReattachTab&) = default;
};
// The user picked a theme; persist it.
struct SetTheme {
    theme::ThemeRef theme;
    friend bool operator==(const SetTheme&, const SetTheme&) = default;
};
// The active tokens changed to this generation; rebuild cached looks.
struct ThemeChanged {
    std::uint64_t generation = 0;
    friend bool operator==(const ThemeChanged&, const ThemeChanged&) = default;
};
} // namespace intent

using Intent = std::variant<intent::Sidebar, intent::CloseProjectTab, intent::DetachTab, intent::ShowDetached,
                            intent::ReattachTab, intent::SetTheme, intent::ThemeChanged>;

} // namespace loa::view::shell
