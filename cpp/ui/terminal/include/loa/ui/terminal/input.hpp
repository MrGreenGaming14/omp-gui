#pragma once

// Key presses and pastes encoded as the bytes a terminal program expects (xterm conventions).
// Port of `crates/loa-ui-terminal/src/input.rs`; keys mirror iced `keyboard::Key` and `Modifiers`.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

namespace loa::ui::terminal {

using Bytes = std::vector<std::uint8_t>;

// The named keys the terminal encodes (iced `key::Named`); `Other` is any key it ignores.
enum class Named : std::uint8_t {
    Enter, Backspace, Tab, Escape, Space, ArrowUp, ArrowDown, ArrowRight, ArrowLeft, Home, End,
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, Insert, Delete, PageUp, PageDown, Other,
};

// iced `keyboard::Key`: a named key, the character a key produces, or an unidentified key.
struct Key {
    enum class Kind : std::uint8_t { Named, Character, Unidentified };
    Kind kind = Kind::Unidentified;
    terminal::Named name = terminal::Named::Other;
    std::string text;

    [[nodiscard]] static Key named(terminal::Named n) { return {Kind::Named, n, {}}; }
    [[nodiscard]] static Key character(std::string c) { return {Kind::Character, terminal::Named::Other, std::move(c)}; }
    [[nodiscard]] static Key unidentified() { return {}; }
    friend bool operator==(const Key&, const Key&) = default;
};

// iced `keyboard::Modifiers` (`LOGO` is Cmd/Super).
struct Modifiers {
    static constexpr std::uint8_t SHIFT = 1, CTRL = 2, ALT = 4, LOGO = 8;
    std::uint8_t bits = 0;

    [[nodiscard]] bool shift() const { return (bits & SHIFT) != 0; }
    [[nodiscard]] bool control() const { return (bits & CTRL) != 0; }
    [[nodiscard]] bool alt() const { return (bits & ALT) != 0; }
    [[nodiscard]] bool logo() const { return (bits & LOGO) != 0; }
    friend bool operator==(const Modifiers&, const Modifiers&) = default;
};

// The terminal modes input depends on (a subset of alacritty `TermMode`).
struct TermMode {
    static constexpr std::uint8_t APP_CURSOR = 1, BRACKETED_PASTE = 2, ALT_SCREEN = 4, ALTERNATE_SCROLL = 8, MOUSE_MODE = 16;
    std::uint8_t bits = 0;

    [[nodiscard]] bool contains(std::uint8_t m) const { return (bits & m) == m; }
    [[nodiscard]] bool intersects(std::uint8_t m) const { return (bits & m) != 0; }
    friend bool operator==(const TermMode&, const TermMode&) = default;
};

// How the viewport moves through the scrollback (alacritty `grid::Scroll`); `Delta` lines are positive up.
struct Scroll {
    enum class Kind : std::uint8_t { Delta, PageUp, PageDown, Top, Bottom };
    Kind kind = Kind::Bottom;
    int delta = 0;

    [[nodiscard]] static Scroll lines(int n) { return {Kind::Delta, n}; }
    [[nodiscard]] static Scroll page_up() { return {Kind::PageUp, 0}; }
    [[nodiscard]] static Scroll page_down() { return {Kind::PageDown, 0}; }
    [[nodiscard]] static Scroll top() { return {Kind::Top, 0}; }
    [[nodiscard]] static Scroll bottom() { return {Kind::Bottom, 0}; }
    friend bool operator==(const Scroll&, const Scroll&) = default;
};

// What a key press does: bytes for the program, or a pane action.
struct KeyAction {
    enum class Kind : std::uint8_t { Send, Copy, Paste, Scroll };
    Kind kind = Kind::Send;
    Bytes bytes;
    terminal::Scroll scroll;

    [[nodiscard]] static KeyAction send(Bytes b) { return {Kind::Send, std::move(b), {}}; }
    [[nodiscard]] static KeyAction copy() { return {Kind::Copy, {}, {}}; }
    [[nodiscard]] static KeyAction paste() { return {Kind::Paste, {}, {}}; }
    [[nodiscard]] static KeyAction scrolled(terminal::Scroll s) { return {Kind::Scroll, {}, s}; }
    friend bool operator==(const KeyAction&, const KeyAction&) = default;
};

// Pane shortcuts first (Ctrl+Shift+C/V or Cmd+C/V, then Shift+Insert/PageUp/PageDown/Home/End), else the
// bytes for the program; `text` is what the key typed.
[[nodiscard]] std::optional<KeyAction> key_action(const Key& key, Modifiers m, std::optional<std::string_view> text, TermMode mode);

// Pasted text: bracketed (`ESC [ 200 ~`..`ESC [ 201 ~`, escapes removed) when the mode is on, else with line
// feeds as Enter's CRs.
[[nodiscard]] Bytes encode_paste(std::string_view text, TermMode mode);

} // namespace loa::ui::terminal
