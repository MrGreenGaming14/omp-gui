#pragma once

// Configs and Settings, each a window over the app, and detached OS windows: a panel tab or a
// subagent's session in a window of its own (port of `loa-app/src/app/windows.rs`). The
// window bookkeeping is plain C++; the Qt host opens the windows these entries name.

#include <array>
#include <cstddef>
#include <cstdint>
#include <filesystem>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/view/theme.hpp"

namespace loa::app {

// An OS window the app opened (iced `window::Id`).
using WindowId = std::uint64_t;

// A section of Configs.
enum class ConfigsTab : std::uint8_t { Models, Tools, Agents, Skills, Rules };

// Configs' sections in navigation order.
inline constexpr std::array<ConfigsTab, 5> CONFIGS_TABS{ConfigsTab::Models, ConfigsTab::Agents, ConfigsTab::Skills,
                                                       ConfigsTab::Tools, ConfigsTab::Rules};
[[nodiscard]] std::string_view blurb(ConfigsTab tab);
// Where omp reads this tab's files from.
[[nodiscard]] std::string_view places(ConfigsTab tab);
[[nodiscard]] std::string_view label(ConfigsTab tab);
[[nodiscard]] std::string_view group(ConfigsTab tab);

// A section of Settings.
enum class SettingsTab : std::uint8_t { General, Peers, Harnesses, Updates };

inline constexpr std::array<SettingsTab, 4> SETTINGS_TABS{SettingsTab::General, SettingsTab::Peers,
                                                         SettingsTab::Harnesses, SettingsTab::Updates};
[[nodiscard]] std::string_view label(SettingsTab tab);

// A Markdown definition file omp reads.
struct DocFile {
    std::string name;
    std::filesystem::path path;
    // "This project" or "All projects".
    std::string_view scope;
    std::string text;
    friend bool operator==(const DocFile&, const DocFile&) = default;
};

// The agent, skill and rule files of the current tab.
struct Docs {
    std::vector<DocFile> files;
    std::size_t selected = 0;
    bool full = false;
};

// Lists the definition files of `tab`, project scope first.
[[nodiscard]] std::vector<DocFile> scan(ConfigsTab tab, const std::filesystem::path& project,
                                        const std::optional<std::filesystem::path>& home);

// A window drawn over the app.
namespace overlay {
struct Usage {
    friend bool operator==(const Usage&, const Usage&) = default;
};
struct Configs {
    ConfigsTab tab;
    friend bool operator==(const Configs&, const Configs&) = default;
};
struct Settings {
    SettingsTab tab;
    friend bool operator==(const Settings&, const Settings&) = default;
};
} // namespace overlay
using Overlay = std::variant<overlay::Usage, overlay::Configs, overlay::Settings>;

// What a detached window shows.
namespace detached {
// A panel tab, by id.
struct Tab {
    std::string id;
    friend bool operator==(const Tab&, const Tab&) = default;
};
// A read-only transcript (e.g. a subagent session file), by key.
struct Transcript {
    std::string key;
    friend bool operator==(const Transcript&, const Transcript&) = default;
};
} // namespace detached
using Detached = std::variant<detached::Tab, detached::Transcript>;

// The size of a new window showing `what`: a wide one for panel tabs other than surfaces.
[[nodiscard]] std::pair<float, float> detached_size(const Detached& what);

// The app's windows: the overlay and the detached ones.
struct Windows {
    std::optional<Overlay> overlay;
    Docs docs;
    std::optional<WindowId> main;
    std::vector<std::pair<WindowId, Detached>> detached;
    // The id the next window opened gets.
    WindowId next = 1;
};

namespace window_msg {
struct OpenConfigs {
    ConfigsTab tab;
};
struct PickDoc {
    std::size_t index = 0;
};
struct ToggleDocFull {};
struct OpenSettings {
    SettingsTab tab;
};
// Closes Configs, Settings or Usage.
struct CloseOverlay {};
struct SetTheme {
    loa::view::theme::ThemeRef theme;
};
// Shows a panel tab in its own window.
struct Detach {
    std::string tab;
};
// Brings a detached tab's window forward.
struct Show {
    std::string tab;
};
// Puts a detached tab back in the panel.
struct ReattachTab {
    std::string tab;
};
// A detached window's Reattach button.
struct Reattach {
    WindowId id = 0;
};
// A window closed (the main one or a detached one).
struct Closed {
    WindowId id = 0;
};
} // namespace window_msg

using WindowMessage =
    std::variant<window_msg::OpenConfigs, window_msg::PickDoc, window_msg::ToggleDocFull, window_msg::OpenSettings,
                 window_msg::CloseOverlay, window_msg::SetTheme, window_msg::Detach, window_msg::Show,
                 window_msg::ReattachTab, window_msg::Reattach, window_msg::Closed>;

} // namespace loa::app
