#pragma once

// Keyboard shortcuts as data: chords bound to `CommandId`s per `Scope`, defaults in one table.
// Port of `loa-ui-shell/src/palette/keymap.rs`. Plain C++; the view turns `QKeyEvent`s into
// `KeyPress`es.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <tuple>
#include <variant>
#include <vector>

#include "loa/ui/shell/command_id.hpp"

namespace loa::ui::shell::palette {

// iced `keyboard::Modifiers`: only Ctrl, Alt, Shift and Logo (Cmd/Super) count.
struct Modifiers {
    std::uint8_t bits = 0;

    static constexpr std::uint8_t CTRL_BIT = 1, ALT_BIT = 2, SHIFT_BIT = 4, LOGO_BIT = 8;
    [[nodiscard]] static constexpr Modifiers none() { return {}; }
    [[nodiscard]] static constexpr Modifiers ctrl() { return {CTRL_BIT}; }
    [[nodiscard]] static constexpr Modifiers alt() { return {ALT_BIT}; }
    [[nodiscard]] static constexpr Modifiers shift() { return {SHIFT_BIT}; }
    [[nodiscard]] static constexpr Modifiers logo() { return {LOGO_BIT}; }

    [[nodiscard]] constexpr bool contains(Modifiers m) const { return (bits & m.bits) == m.bits; }
    [[nodiscard]] constexpr bool empty() const { return bits == 0; }
    friend constexpr Modifiers operator|(Modifiers a, Modifiers b) { return {static_cast<std::uint8_t>(a.bits | b.bits)}; }
    friend constexpr Modifiers operator&(Modifiers a, Modifiers b) { return {static_cast<std::uint8_t>(a.bits & b.bits)}; }
    [[nodiscard]] constexpr Modifiers difference(Modifiers m) const { return {static_cast<std::uint8_t>(bits & ~m.bits)}; }
    friend constexpr bool operator==(Modifiers, Modifiers) = default;
};

// iced `key::Named`, the subset the app can bind or must ignore.
enum class Named : std::uint8_t {
    Escape, Enter, Tab, Space, Backspace, Delete, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, PageUp, PageDown, Home, End,
    Insert, F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
    // Modifier keys: pressing one alone is never a chord.
    Shift, Control, Alt, AltGraph, Super, Meta, Hyper,
};
// iced's `Debug` name of `n` (`F5`, `Insert`).
[[nodiscard]] std::string_view name(Named n);

enum class Platform : std::uint8_t { MacOs, Other };
// The platform this binary runs on.
[[nodiscard]] constexpr Platform current_platform() {
#if defined(__APPLE__)
    return Platform::MacOs;
#else
    return Platform::Other;
#endif
}
// Cmd on macOS, Ctrl elsewhere.
[[nodiscard]] constexpr Modifiers primary(Platform p) { return p == Platform::MacOs ? Modifiers::logo() : Modifiers::ctrl(); }

// The non-modifier key of a chord: a lowercase latin character or a named key.
using ChordKey = std::variant<char32_t, Named>;

// A key pressed, as the view reports it (iced `keyboard::Event::KeyPressed`).
struct KeyPress {
    struct Unidentified {
        friend bool operator==(const Unidentified&, const Unidentified&) = default;
    };
    // The logical key: named, the UTF-8 text it types, or unknown.
    std::variant<Named, std::string, Unidentified> key;
    // The latin character at the physical key's position, if any (iced `Key::to_latin`).
    std::optional<char32_t> latin;
    Modifiers modifiers;
    bool repeat = false;
};

// A key pressed with exactly these modifiers.
struct Chord {
    ChordKey key;
    Modifiers modifiers;

    // Lowercases a character key and keeps only Ctrl, Alt, Shift and Logo.
    [[nodiscard]] static Chord make(ChordKey key, Modifiers modifiers);
    [[nodiscard]] static Chord character(char32_t c, Modifiers m) { return make(c, m); }
    [[nodiscard]] static Chord named(Named n, Modifiers m) { return make(n, m); }
    // The chord of a key press, with non-latin characters mapped to the latin key at the same position.
    [[nodiscard]] static std::optional<Chord> from_event(const KeyPress& event);
    // The hint shown next to a command: `Ctrl+K`, or `⌘K` on macOS.
    [[nodiscard]] std::string label(Platform platform) const;

    friend bool operator==(const Chord&, const Chord&) = default;
};

// Where a binding applies: everywhere, in the composer (over global bindings), or in the modal palette.
enum class Scope : std::uint8_t { Global, Composer, Palette };

class Keymap {
  public:
    using Platform = palette::Platform;
    using Chord = palette::Chord;
    using ChordKey = palette::ChordKey;
    using Scope = palette::Scope;
    using Binding = std::tuple<Chord, Scope, CommandId>;

    // The default bindings, with Cmd on macOS and Ctrl elsewhere.
    [[nodiscard]] static Keymap defaults(Platform platform);

    [[nodiscard]] Platform platform() const { return platform_; }
    [[nodiscard]] const std::vector<Binding>& bindings() const { return bindings_; }
    // The command a key press runs in `scope`: its own binding, else (for the composer) the
    // global one. Repeats run nothing.
    [[nodiscard]] const CommandId* resolve(const KeyPress& event, Scope scope) const;
    // The hint for `command`: its global chord, else its first scoped one.
    [[nodiscard]] std::optional<Chord> chord_for(const CommandId& command) const;

  private:
    Platform platform_ = Platform::Other;
    std::vector<Binding> bindings_;
};

} // namespace loa::ui::shell::palette

namespace loa::ui::shell {
using palette::Keymap;
} // namespace loa::ui::shell
