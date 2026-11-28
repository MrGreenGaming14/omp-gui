#pragma once

// Command ids of the palette and keymap; the strings match `loa-ui-shell` byte for byte.

#include <compare>
#include <cstddef>
#include <string>
#include <string_view>

namespace loa::ui::shell::palette {

class CommandId {
  public:
    CommandId() = default;
    explicit CommandId(std::string id) : id_(std::move(id)) {}
    explicit CommandId(std::string_view id) : id_(id) {}
    explicit CommandId(const char* id) : id_(id) {}

    [[nodiscard]] const std::string& as_str() const { return id_; }

    friend bool operator==(const CommandId&, const CommandId&) = default;
    friend std::strong_ordering operator<=>(const CommandId&, const CommandId&) = default;

  private:
    std::string id_;
};

namespace ids {
inline constexpr std::string_view PALETTE_TOGGLE = "palette.toggle";
inline constexpr std::string_view PANEL_TOGGLE = "panel.toggle";
inline constexpr std::string_view PANEL_AGENTS = "panel.agents";
inline constexpr std::string_view SESSION_INTERRUPT = "session.interrupt";
inline constexpr std::string_view COMPOSER_SUBMIT = "composer.submit";
inline constexpr std::string_view COMPOSER_NEWLINE = "composer.newline";
inline constexpr std::string_view SESSION_NEW = "session.new";
inline constexpr std::string_view SESSION_NEXT = "session.next";
inline constexpr std::string_view SESSION_PREVIOUS = "session.previous";
inline constexpr std::string_view PROJECT_TAB_NEXT = "project.tab.next";
inline constexpr std::string_view PROJECT_TAB_PREVIOUS = "project.tab.previous";
inline constexpr std::string_view COMPOSER_FOCUS = "composer.focus";
inline constexpr std::string_view SEARCH_FOCUS = "sidebar.search";
inline constexpr std::string_view KEYMAP_SHOW = "keymap.show";
inline constexpr std::string_view FOCUS_NEXT = "focus.next";
inline constexpr std::string_view FOCUS_PREVIOUS = "focus.previous";

// Prefixes of `session.goto.<n>` (the n-th session in the sidebar) and `panel.tab.<n>`, 1 based.
inline constexpr std::string_view SESSION_GOTO = "session.goto.";
inline constexpr std::string_view PANEL_TAB = "panel.tab.";

[[nodiscard]] inline CommandId session_goto(std::size_t n) { return CommandId(std::string(SESSION_GOTO) + std::to_string(n)); }
[[nodiscard]] inline CommandId panel_tab(std::size_t n) { return CommandId(std::string(PANEL_TAB) + std::to_string(n)); }
} // namespace ids

} // namespace loa::ui::shell::palette
