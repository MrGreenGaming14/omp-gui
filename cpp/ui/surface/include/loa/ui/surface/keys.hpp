#pragma once

// Translating Qt keys, buttons and wheel movements into the surface contract's W3C terms. Port
// of `crates/loa-ui-surface/src/keys.rs`. Qt values come in as plain integers (`Qt::Key`,
// `Qt::KeyboardModifiers`, `Qt::MouseButton`, X11/evdev scan codes), so this header stays Qt-free.

#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <utility>
#include <variant>
#include <vector>

#include "loa/view/surface.hpp"

namespace loa::ui::surface {

using loa::view::surface::Input;
using loa::view::surface::Modifiers;
using loa::view::surface::MouseButton;
// The contract's input kinds (`input::Key`, `input::Text`, …).
namespace input = loa::contracts::surface::input;

// Pixels one wheel line scrolls.
inline constexpr float LINE_PX = 40.0F;

// A key press or release, already in W3C terms.
struct KeyInput {
    // `KeyboardEvent.key`.
    std::string key;
    // `KeyboardEvent.code`.
    std::string code;
    bool pressed = false;
    Modifiers modifiers;
    // Text the press produced, if any.
    std::optional<std::string> text;

    // What to send for this key. `held` holds the codes whose press went out as `input::Key`.
    //
    // Typing goes as committed `input::Text` and its release is dropped, so the server sees each
    // character once. Keys that type nothing (arrows, Enter, chords with Ctrl, Alt or Meta) go as
    // `input::Key`, and their releases follow even when the modifier was let go first.
    [[nodiscard]] std::optional<Input> to_input(std::vector<std::string>& held) const;

    friend bool operator==(const KeyInput&, const KeyInput&) = default;
};

// A wheel movement as Qt reports it: whole lines (angle delta / 120) or pixels.
namespace scroll_delta {
struct Lines {
    float x = 0, y = 0;
};
struct Pixels {
    float x = 0, y = 0;
};
} // namespace scroll_delta
using ScrollDelta = std::variant<scroll_delta::Lines, scroll_delta::Pixels>;

// The Qt to W3C key tables.
struct KeyMap {
    // The W3C `KeyboardEvent.key` of `Qt::Key` `qt_key` with produced `text`: named keys by their
    // W3C name (Space is " ", Super is "Meta"), characters as typed, else "Unidentified".
    [[nodiscard]] static std::string key(int qt_key, std::string_view text);
    // The W3C `KeyboardEvent.code` of an X11 key code (evdev + 8, `QKeyEvent::nativeScanCode`);
    // "Unidentified" when unknown.
    [[nodiscard]] static std::string code(std::uint32_t native_scan_code);
    // `Qt::KeyboardModifiers` bits as contract modifiers (Qt's Meta is the logo key).
    [[nodiscard]] static Modifiers modifiers(int qt_modifiers);
    // A `Qt::MouseButton`; `nullopt` for buttons the contract has no name for.
    [[nodiscard]] static std::optional<MouseButton> button(int qt_button);
    // The key input of a Qt key event. Modifier keys are keys too (iced reports them as presses
    // beside `ModifiersChanged`).
    [[nodiscard]] static KeyInput from_qt(int qt_key, std::uint32_t native_scan_code, int qt_modifiers, std::string_view text,
                                          bool pressed);
    // A wheel movement in panel pixels, positive `dy` scrolling the content down as the contract
    // wants. Qt (like iced) reports wheel up as positive.
    [[nodiscard]] static std::pair<float, float> scroll_pixels(ScrollDelta delta);
};

} // namespace loa::ui::surface
