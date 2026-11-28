#pragma once

// The app frame: sidebar, top bar over a center slot, and a resizable right panel with tabs.
// Port of `loa-ui-shell/src/layout.rs` (and the top bar constant of `topbar.rs`).

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/ui/shell/sidebar.hpp"
#include "loa/ui/shell/task.hpp"
#include "loa/ui/shell/theme.hpp"
#include "loa/view/shell.hpp"

namespace loa::ui::shell {

namespace topbar {
// Height of the top bar, shared with the panel tab bar so their borders line up.
inline constexpr float HEIGHT = 52.0F;
} // namespace topbar

namespace layout {

using loa::view::shell::Intent;
using loa::view::shell::NodeStatus;
using loa::view::shell::PanelTab;
using loa::view::shell::SessionSummary;

inline constexpr float SIDEBAR_WIDTH = 272.0F;
inline constexpr float SIDEBAR_MIN_WIDTH = 200.0F;
inline constexpr float SIDEBAR_MAX_WIDTH = 480.0F;
inline constexpr float MIN_PANEL_WIDTH = 240.0F;
inline constexpr float DEFAULT_PANEL_WIDTH = 420.0F;
inline constexpr float MIN_CENTER_WIDTH = 320.0F;
inline constexpr std::string_view PANEL_TABS_ID = "loa-panel-tabs";
inline constexpr std::string_view PROJECT_TABS_ID = "loa-project-tabs";
inline constexpr std::string_view PANEL_DETACH_ID = "loa-panel-detach";
inline constexpr float PROJECT_TABS_HEIGHT = 34.0F;
// Pixels a mouse wheel line scrolls a tab strip.
inline constexpr float WHEEL_LINE = 40.0F;

// A pane that can fill the window, or whose edge is dragged.
enum class Pane : std::uint8_t { Sidebar, Center, Panel };

struct Size {
    float width = 0, height = 0;
    friend bool operator==(const Size&, const Size&) = default;
};

// iced `mouse::ScrollDelta`.
struct ScrollDelta {
    enum class Unit : std::uint8_t { Lines, Pixels };
    Unit unit = Unit::Lines;
    float x = 0, y = 0;
    friend bool operator==(const ScrollDelta&, const ScrollDelta&) = default;
};

namespace msg {
// Pass this intent on as is.
struct Emit {
    Intent intent;
    friend bool operator==(const Emit&, const Emit&) = default;
};
struct Sidebar {
    sidebar::Message message;
    friend bool operator==(const Sidebar&, const Sidebar&) = default;
};
struct SelectTab {
    std::string id;
    friend bool operator==(const SelectTab&, const SelectTab&) = default;
};
struct TabsWheel {
    std::string id;
    ScrollDelta delta;
    friend bool operator==(const TabsWheel&, const TabsWheel&) = default;
};
struct DragStart {
    Pane pane;
    friend bool operator==(const DragStart&, const DragStart&) = default;
};
// The cursor moved to this x while an edge is held.
struct DragMove {
    float x = 0;
    friend bool operator==(const DragMove&, const DragMove&) = default;
};
struct DragEnd {
    friend bool operator==(const DragEnd&, const DragEnd&) = default;
};
struct TogglePanel {
    friend bool operator==(const TogglePanel&, const TogglePanel&) = default;
};
struct ToggleFull {
    Pane pane;
    friend bool operator==(const ToggleFull&, const ToggleFull&) = default;
};
struct FrameSized {
    Size size;
    friend bool operator==(const FrameSized&, const FrameSized&) = default;
};
struct SetTheme {
    loa::view::theme::ThemeRef theme;
    friend bool operator==(const SetTheme&, const SetTheme&) = default;
};
struct SystemTheme {
    theme::Mode mode;
    friend bool operator==(const SystemTheme&, const SystemTheme&) = default;
};
} // namespace msg

using Message = std::variant<msg::Emit, msg::Sidebar, msg::SelectTab, msg::TabsWheel, msg::DragStart, msg::DragMove, msg::DragEnd,
                             msg::TogglePanel, msg::ToggleFull, msg::FrameSized, msg::SetTheme, msg::SystemTheme>;
using Event = Intent;

// The message that opens `project`'s page.
[[nodiscard]] Message open_project(std::string project);

class State {
  public:
    // The frame with the panel open; the task asks the system for its theme.
    [[nodiscard]] static std::pair<State, Task> make(loa::view::theme::Appearance appearance);

    [[nodiscard]] const theme::State& theme_state() const { return theme_; }
    // Replaces the appearance (theme and overrides), e.g. after the config changed.
    std::optional<Intent> set_appearance(loa::view::theme::Appearance appearance);
    [[nodiscard]] const sidebar::State& sidebar() const { return sidebar_; }
    [[nodiscard]] sidebar::State& sidebar_mut() { return sidebar_; }
    // Highlights the selected project in the sidebar and the project tab strip.
    void set_active_project(std::optional<std::string> p) { sidebar_.active_project = std::move(p); }
    void set_shown_project(std::optional<std::string> p) { sidebar_.shown_project = std::move(p); }
    void set_project_tabs(bool enabled) { project_tabs_ = enabled; }
    [[nodiscard]] bool project_tabs() const { return project_tabs_; }
    void set_open_projects(std::vector<std::string> projects) { open_projects_ = std::move(projects); }
    [[nodiscard]] const std::vector<std::string>& open_projects() const { return open_projects_; }
    [[nodiscard]] std::optional<Pane> full() const { return full_; }
    [[nodiscard]] std::optional<Pane> dragging() const { return dragging_; }
    void set_detached(std::vector<std::string> tabs) { detached_ = std::move(tabs); }
    [[nodiscard]] bool is_detached(std::string_view tab) const;
    // The short status of the shown session in the top bar, e.g. "Running".
    void set_session_status(std::optional<std::string> status) { status_ = std::move(status); }
    [[nodiscard]] const std::optional<std::string>& session_status() const { return status_; }
    // Marks whether session `id` runs a turn in the sidebar.
    void set_session_running(std::string_view id, bool running) { sidebar_.set_running(id, running); }
    // Replaces the panel tabs; the active tab stays when it is still there.
    void set_tabs(std::vector<PanelTab> tabs) { tabs_ = std::move(tabs); }
    [[nodiscard]] const std::vector<PanelTab>& tabs() const { return tabs_; }
    [[nodiscard]] bool panel_open() const { return panel_open_; }
    [[nodiscard]] float panel_width() const { return panel_width_; }
    [[nodiscard]] float sidebar_width() const { return sidebar_width_; }
    // The panel width as drawn: the user's, capped so the center keeps its minimum.
    [[nodiscard]] float shown_panel_width() const;
    // The active session and its node, as the top bar shows them.
    [[nodiscard]] std::optional<std::pair<const SessionSummary*, NodeStatus>> breadcrumb() const;
    // Adds a tab, or replaces the tab with the same id.
    void add_tab(PanelTab tab);
    // Opens the panel on this tab; pair it with `reveal_active_tab`.
    void show_tab(std::string_view id);
    // Snaps an overflowing tab bar to the active tab's share of it, so the first and last tabs land at its ends.
    [[nodiscard]] Task reveal_active_tab() const;
    [[nodiscard]] std::optional<std::string_view> active_tab() const;
    // Opens or closes the panel, keeping width and tab; closing leaves its full screen.
    void set_panel_open(bool open);

    std::pair<Task, std::optional<Event>> update(Message message);

  private:
    explicit State(loa::view::theme::Appearance appearance) : theme_(std::move(appearance)) {}
    [[nodiscard]] float max_panel_width() const;

    sidebar::State sidebar_;
    theme::State theme_;
    std::optional<std::string> status_;
    bool panel_open_ = true;
    float panel_width_ = DEFAULT_PANEL_WIDTH;
    std::vector<PanelTab> tabs_;
    // The tab the user picked; may name a tab not added yet, then the first tab shows.
    std::optional<std::string> active_;
    std::optional<Size> frame_;
    bool project_tabs_ = false;
    std::vector<std::string> open_projects_;
    float sidebar_width_ = SIDEBAR_WIDTH;
    std::optional<Pane> dragging_;
    std::optional<Pane> full_;
    // Tabs shown in their own window instead of the panel.
    std::vector<std::string> detached_;
};

// The tab strip scroll a wheel delta asks for.
[[nodiscard]] Task wheel_tabs(std::string_view id, ScrollDelta delta);

} // namespace layout

} // namespace loa::ui::shell
