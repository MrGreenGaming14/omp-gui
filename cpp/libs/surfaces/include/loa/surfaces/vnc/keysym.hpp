#pragma once

// W3C `KeyboardEvent.key` values to X11 keysyms, as RFB KeyEvents carry them.
// Port of `loa-surfaces/src/vnc/keysym.rs`.

#include <cstdint>
#include <optional>
#include <string_view>

namespace loa::surfaces::vnc::keysym {

inline constexpr std::uint32_t SHIFT_L = 0xffe1;
inline constexpr std::uint32_t CONTROL_L = 0xffe3;
inline constexpr std::uint32_t ALT_L = 0xffe9;
inline constexpr std::uint32_t SUPER_L = 0xffeb;
inline constexpr std::uint32_t RETURN = 0xff0d;
inline constexpr std::uint32_t TAB = 0xff09;
inline constexpr std::uint32_t BACKSPACE = 0xff08;

// The keysym for a key event (`key` and `code` as UTF-8), or `std::nullopt`
// when the key has no keysym (`Unidentified`, `Dead`, unknown names).
[[nodiscard]] std::optional<std::uint32_t> keysym(std::string_view key, std::string_view code);

// The keysym typing the code point `c` produces. Latin-1 characters map to
// themselves, other characters to the Unicode keysym range, and the control
// characters a text field can hold to their editing keys.
[[nodiscard]] std::optional<std::uint32_t> char_keysym(char32_t c);

// Whether `key` is one of the four modifiers `Modifiers` tracks.
[[nodiscard]] bool is_modifier(std::string_view key);

} // namespace loa::surfaces::vnc::keysym
