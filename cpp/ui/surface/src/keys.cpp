#include "loa/ui/surface/keys.hpp"

#include <QtCore/qnamespace.h>

#include <algorithm>
#include <array>
#include <cctype>

namespace loa::ui::surface {

namespace {

// Text with a control character (C0, DEL or C1) types nothing visible.
bool has_control(std::string_view s) {
    for (std::size_t i = 0; i < s.size(); ++i) {
        const auto c = static_cast<unsigned char>(s[i]);
        if (c < 0x20 || c == 0x7f) return true;
        if (c == 0xc2 && i + 1 < s.size()) {
            const auto n = static_cast<unsigned char>(s[i + 1]);
            if (n >= 0x80 && n <= 0x9f) return true;
        }
    }
    return false;
}

struct Named {
    int qt;
    std::string_view w3c;
};

// `Qt::Key` to the W3C named key values iced reports (its `Named` debug names).
constexpr std::array NAMED{
    Named{Qt::Key_Space, " "},
    Named{Qt::Key_Escape, "Escape"},
    Named{Qt::Key_Tab, "Tab"},
    Named{Qt::Key_Backtab, "Tab"},
    Named{Qt::Key_Backspace, "Backspace"},
    Named{Qt::Key_Return, "Enter"},
    Named{Qt::Key_Enter, "Enter"},
    Named{Qt::Key_Insert, "Insert"},
    Named{Qt::Key_Delete, "Delete"},
    Named{Qt::Key_Pause, "Pause"},
    Named{Qt::Key_Print, "PrintScreen"},
    Named{Qt::Key_Clear, "Clear"},
    Named{Qt::Key_Home, "Home"},
    Named{Qt::Key_End, "End"},
    Named{Qt::Key_Left, "ArrowLeft"},
    Named{Qt::Key_Up, "ArrowUp"},
    Named{Qt::Key_Right, "ArrowRight"},
    Named{Qt::Key_Down, "ArrowDown"},
    Named{Qt::Key_PageUp, "PageUp"},
    Named{Qt::Key_PageDown, "PageDown"},
    Named{Qt::Key_Shift, "Shift"},
    Named{Qt::Key_Control, "Control"},
    Named{Qt::Key_Meta, "Meta"},
    Named{Qt::Key_Super_L, "Meta"},
    Named{Qt::Key_Super_R, "Meta"},
    Named{Qt::Key_Alt, "Alt"},
    Named{Qt::Key_AltGr, "AltGraph"},
    Named{Qt::Key_CapsLock, "CapsLock"},
    Named{Qt::Key_NumLock, "NumLock"},
    Named{Qt::Key_ScrollLock, "ScrollLock"},
    Named{Qt::Key_Menu, "ContextMenu"},
    Named{Qt::Key_Hyper_L, "Hyper"},
    Named{Qt::Key_Hyper_R, "Hyper"},
    Named{Qt::Key_Help, "Help"},
    Named{Qt::Key_VolumeDown, "AudioVolumeDown"},
    Named{Qt::Key_VolumeUp, "AudioVolumeUp"},
    Named{Qt::Key_VolumeMute, "AudioVolumeMute"},
    Named{Qt::Key_MediaPlay, "MediaPlay"},
    Named{Qt::Key_MediaStop, "MediaStop"},
    Named{Qt::Key_MediaPrevious, "MediaTrackPrevious"},
    Named{Qt::Key_MediaNext, "MediaTrackNext"},
    Named{Qt::Key_MediaPause, "MediaPause"},
    Named{Qt::Key_MediaTogglePlayPause, "MediaPlayPause"},
    Named{Qt::Key_Back, "BrowserBack"},
    Named{Qt::Key_Forward, "BrowserForward"},
    Named{Qt::Key_Refresh, "BrowserRefresh"},
    Named{Qt::Key_Copy, "Copy"},
    Named{Qt::Key_Cut, "Cut"},
    Named{Qt::Key_Paste, "Paste"},
};

// Linux evdev key codes (`input-event-codes.h`) to W3C `KeyboardEvent.code`.
std::string_view evdev_code(std::uint32_t k) {
    static constexpr std::array<std::string_view, 128> LOW{
        "", "Escape", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8", "Digit9", "Digit0",
        "Minus", "Equal", "Backspace", "Tab", "KeyQ", "KeyW", "KeyE", "KeyR", "KeyT", "KeyY", "KeyU", "KeyI", "KeyO", "KeyP",
        "BracketLeft", "BracketRight", "Enter", "ControlLeft", "KeyA", "KeyS", "KeyD", "KeyF", "KeyG", "KeyH", "KeyJ", "KeyK",
        "KeyL", "Semicolon", "Quote", "Backquote", "ShiftLeft", "Backslash", "KeyZ", "KeyX", "KeyC", "KeyV", "KeyB", "KeyN",
        "KeyM", "Comma", "Period", "Slash", "ShiftRight", "NumpadMultiply", "AltLeft", "Space", "CapsLock", "F1", "F2", "F3",
        "F4", "F5", "F6", "F7", "F8", "F9", "F10", "NumLock", "ScrollLock", "Numpad7", "Numpad8", "Numpad9", "NumpadSubtract",
        "Numpad4", "Numpad5", "Numpad6", "NumpadAdd", "Numpad1", "Numpad2", "Numpad3", "Numpad0", "NumpadDecimal", "",
        "Lang5", "IntlBackslash", "F11", "F12", "IntlRo", "Lang3", "Lang4", "Convert", "KanaMode", "NonConvert", "",
        "NumpadEnter", "ControlRight", "NumpadDivide", "PrintScreen", "AltRight", "", "Home", "ArrowUp", "PageUp", "ArrowLeft",
        "ArrowRight", "End", "ArrowDown", "PageDown", "Insert", "Delete", "", "AudioVolumeMute", "AudioVolumeDown",
        "AudioVolumeUp", "Power", "NumpadEqual", "", "Pause", "", "NumpadComma", "Lang1", "Lang2", "IntlYen", "MetaLeft",
        "MetaRight", "ContextMenu"};
    if (k < LOW.size()) return LOW[k];
    if (k >= 183 && k <= 194) {
        static constexpr std::array<std::string_view, 12> F{"F13", "F14", "F15", "F16", "F17", "F18",
                                                             "F19", "F20", "F21", "F22", "F23", "F24"};
        return F[k - 183];
    }
    return {};
}

} // namespace

std::optional<Input> KeyInput::to_input(std::vector<std::string>& held) const {
    namespace in = loa::ui::surface::input;
    if (pressed) {
        const bool chord = modifiers.ctrl || modifiers.alt || modifiers.meta;
        if (text && !chord && !text->empty() && !has_control(*text)) return Input{in::Text{*text}};
        if (std::ranges::find(held, code) == held.end()) held.push_back(code);
    } else {
        auto it = std::ranges::find(held, code);
        if (it == held.end()) return std::nullopt;
        // `swap_remove`.
        *it = std::move(held.back());
        held.pop_back();
    }
    return Input{in::Key{key, code, pressed, modifiers}};
}

std::string KeyMap::key(int qt_key, std::string_view text) {
    for (const auto& n : NAMED)
        if (n.qt == qt_key) return std::string(n.w3c);
    if (qt_key >= Qt::Key_F1 && qt_key <= Qt::Key_F35) return "F" + std::to_string(qt_key - Qt::Key_F1 + 1);
    if (!text.empty() && !has_control(text)) return std::string(text);
    // Chords (Ctrl+C) type a control character: the key is the character without it.
    if (qt_key > 0x20 && qt_key < 0x7f) {
        const char c = static_cast<char>(qt_key);
        return std::string(1, static_cast<char>(std::tolower(static_cast<unsigned char>(c))));
    }
    if (qt_key > 0x7f && qt_key < 0x01000000) {
        // A Latin-1 character key; as UTF-8.
        const auto cp = static_cast<unsigned>(qt_key);
        if (cp < 0x800) return {static_cast<char>(0xc0 | (cp >> 6)), static_cast<char>(0x80 | (cp & 0x3f))};
    }
    return "Unidentified";
}

std::string KeyMap::code(std::uint32_t native_scan_code) {
#if defined(__linux__)
    // X11 and XWayland key codes are evdev codes plus 8.
    if (native_scan_code >= 8) {
        auto c = evdev_code(native_scan_code - 8);
        if (!c.empty()) return std::string(c);
    }
#else
    (void)native_scan_code;
#endif
    return "Unidentified";
}

Modifiers KeyMap::modifiers(int m) {
    return Modifiers{(m & Qt::ShiftModifier) != 0, (m & Qt::ControlModifier) != 0, (m & Qt::AltModifier) != 0,
                     (m & Qt::MetaModifier) != 0};
}

std::optional<MouseButton> KeyMap::button(int b) {
    switch (b) {
    case Qt::LeftButton: return MouseButton::Left;
    case Qt::RightButton: return MouseButton::Right;
    case Qt::MiddleButton: return MouseButton::Middle;
    case Qt::BackButton: return MouseButton::Back;
    case Qt::ForwardButton: return MouseButton::Forward;
    default: return std::nullopt;
    }
}

std::pair<float, float> KeyMap::scroll_pixels(ScrollDelta delta) {
    if (const auto* l = std::get_if<scroll_delta::Lines>(&delta)) return {-l->x * LINE_PX, -l->y * LINE_PX};
    const auto& p = std::get<scroll_delta::Pixels>(delta);
    return {-p.x, -p.y};
}

KeyInput KeyMap::from_qt(int qt_key, std::uint32_t native_scan_code, int qt_modifiers, std::string_view text, bool pressed) {
    KeyInput k{key(qt_key, text), code(native_scan_code), pressed, modifiers(qt_modifiers), std::nullopt};
    if (pressed && !text.empty()) k.text = std::string(text);
    return k;
}

} // namespace loa::ui::surface
