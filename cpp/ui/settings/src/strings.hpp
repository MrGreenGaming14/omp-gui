#pragma once

// Rust `str::trim`/`trim_end` over ASCII and Unicode-agnostic whitespace bytes, as the screens
// trim user input. Internal to `loa_ui_settings`.

#include <string>
#include <string_view>

namespace loa::ui::settings::str {

inline bool space(char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\v' || c == '\f'; }

inline std::string_view trim_end(std::string_view s) {
    while (!s.empty() && space(s.back())) s.remove_suffix(1);
    return s;
}

inline std::string_view trim(std::string_view s) {
    while (!s.empty() && space(s.front())) s.remove_prefix(1);
    return trim_end(s);
}

inline std::string trimmed(std::string_view s) { return std::string(trim(s)); }

} // namespace loa::ui::settings::str
