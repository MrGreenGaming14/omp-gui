// W3C key values to X11 keysyms. Port of `loa-surfaces/src/vnc/keysym.rs`.

#include "loa/surfaces/vnc/keysym.hpp"

#include <array>
#include <charconv>
#include <utility>

#include "internal.hpp"

namespace loa::surfaces::vnc::keysym {

namespace {

// Named keys with no left or right variant.
constexpr std::array<std::pair<std::string_view, std::uint32_t>, 29> NAMED = {{
    {"Backspace", BACKSPACE}, {"Tab", TAB}, {"Clear", 0xff0b}, {"Enter", RETURN}, {"Pause", 0xff13},
    {"ScrollLock", 0xff14}, {"Escape", 0xff1b}, {"Home", 0xff50}, {"ArrowLeft", 0xff51}, {"ArrowUp", 0xff52},
    {"ArrowRight", 0xff53}, {"ArrowDown", 0xff54}, {"PageUp", 0xff55}, {"PageDown", 0xff56}, {"End", 0xff57},
    {"Select", 0xff60}, {"PrintScreen", 0xff61}, {"Execute", 0xff62}, {"Insert", 0xff63}, {"Undo", 0xff65},
    {"Redo", 0xff66}, {"ContextMenu", 0xff67}, {"Find", 0xff68}, {"Cancel", 0xff69}, {"Help", 0xff6a},
    {"NumLock", 0xff7f}, {"CapsLock", 0xffe5}, {"AltGraph", 0xfe03}, {"Delete", 0xffff},
}};

// Modifier keys: W3C key, left keysym, right keysym.
struct Modifier {
    std::string_view key;
    std::uint32_t left;
    std::uint32_t right;
};
constexpr std::array<Modifier, 4> MODIFIERS = {{
    {"Shift", SHIFT_L, 0xffe2},
    {"Control", CONTROL_L, 0xffe4},
    {"Alt", ALT_L, 0xffea},
    {"Meta", SUPER_L, 0xffec},
}};

// Keypad keys by their W3C `key` value, used when `code` names a numpad key.
constexpr std::array<std::pair<std::string_view, std::uint32_t>, 8> KEYPAD = {{
    {"Enter", 0xff8d}, {"*", 0xffaa}, {"+", 0xffab}, {",", 0xffac}, {"-", 0xffad}, {".", 0xffae}, {"/", 0xffaf}, {"=", 0xffbd},
}};

// Keysym of the first function key, F1. F2 to F35 follow it.
constexpr std::uint32_t F1 = 0xffbe;

} // namespace

std::optional<std::uint32_t> keysym(std::string_view key, std::string_view code) {
    if (code.starts_with("Numpad")) {
        for (const auto& [k, sym] : KEYPAD) {
            if (k == key) return sym;
        }
        if (key.size() == 1 && key[0] >= '0' && key[0] <= '9') return 0xffb0 + std::uint32_t(key[0] - '0');
    }
    const std::u32string chars = detail::utf8_decode(key);
    if (chars.size() == 1) return char_keysym(chars[0]);
    for (const auto& [k, sym] : NAMED) {
        if (k == key) return sym;
    }
    for (const auto& m : MODIFIERS) {
        if (m.key == key) return code.ends_with("Right") ? m.right : m.left;
    }
    if (!key.starts_with('F')) return std::nullopt;
    std::string_view digits = key.substr(1);
    if (digits.starts_with('+')) digits.remove_prefix(1);
    std::uint32_t n = 0;
    const auto [end, ec] = std::from_chars(digits.data(), digits.data() + digits.size(), n);
    if (digits.empty() || ec != std::errc{} || end != digits.data() + digits.size()) return std::nullopt;
    if (n < 1 || n > 35) return std::nullopt;
    return F1 + n - 1;
}

std::optional<std::uint32_t> char_keysym(char32_t c) {
    const auto cp = std::uint32_t(c);
    if (c == U'\n' || c == U'\r') return RETURN;
    if (c == U'\t') return TAB;
    if (c == U'\b') return BACKSPACE;
    if ((cp >= 0x20 && cp <= 0x7e) || (cp >= 0xa0 && cp <= 0xff)) return cp;
    if (cp < 0x20 || (cp >= 0x7f && cp < 0xa0)) return std::nullopt;
    return 0x0100'0000 + cp;
}

bool is_modifier(std::string_view key) {
    for (const auto& m : MODIFIERS) {
        if (m.key == key) return true;
    }
    return false;
}

} // namespace loa::surfaces::vnc::keysym
