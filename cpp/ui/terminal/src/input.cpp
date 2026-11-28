#include "loa/ui/terminal/input.hpp"

#include <algorithm>
#include <array>
#include <cctype>
#include <string>

namespace loa::ui::terminal {

namespace {

// A named key: fixed bytes (plain, Ctrl), `ESC [ x` (`ESC O x` in app cursor mode), `ESC O x` or `ESC [ n ~`;
// modifiers add `;m`.
struct Seq {
    enum class Kind : std::uint8_t { Text, Cursor, Ss3, Tilde };
    Kind kind;
    std::string_view plain;
    std::string_view ctrl;
    std::uint8_t code;
};

constexpr Seq text(std::string_view plain, std::string_view ctrl) { return {Seq::Kind::Text, plain, ctrl, 0}; }
constexpr Seq cursor(char x) { return {Seq::Kind::Cursor, {}, {}, static_cast<std::uint8_t>(x)}; }
constexpr Seq ss3(char x) { return {Seq::Kind::Ss3, {}, {}, static_cast<std::uint8_t>(x)}; }
constexpr Seq tilde(std::uint8_t n) { return {Seq::Kind::Tilde, {}, {}, n}; }

using std::string_view_literals::operator""sv;

const std::array<std::pair<Named, Seq>, 27> NAMED{{
    {Named::Enter, text("\r", "\r")}, {Named::Backspace, text("\x7f", "\x08")}, {Named::Tab, text("\t", "\t")},
    {Named::Escape, text("\x1b", "\x1b")}, {Named::Space, text(" ", "\0"sv)},
    {Named::ArrowUp, cursor('A')}, {Named::ArrowDown, cursor('B')}, {Named::ArrowRight, cursor('C')}, {Named::ArrowLeft, cursor('D')},
    {Named::Home, cursor('H')}, {Named::End, cursor('F')},
    {Named::F1, ss3('P')}, {Named::F2, ss3('Q')}, {Named::F3, ss3('R')}, {Named::F4, ss3('S')},
    {Named::Insert, tilde(2)}, {Named::Delete, tilde(3)}, {Named::PageUp, tilde(5)}, {Named::PageDown, tilde(6)},
    {Named::F5, tilde(15)}, {Named::F6, tilde(17)}, {Named::F7, tilde(18)}, {Named::F8, tilde(19)},
    {Named::F9, tilde(20)}, {Named::F10, tilde(21)}, {Named::F11, tilde(23)}, {Named::F12, tilde(24)},
}};

// Shift plus these keys act on the pane: paste and the scrollback.
std::optional<KeyAction> shifted(Named n) {
    switch (n) {
    case Named::Insert: return KeyAction::paste();
    case Named::PageUp: return KeyAction::scrolled(Scroll::page_up());
    case Named::PageDown: return KeyAction::scrolled(Scroll::page_down());
    case Named::Home: return KeyAction::scrolled(Scroll::top());
    case Named::End: return KeyAction::scrolled(Scroll::bottom());
    default: return std::nullopt;
    }
}

// Ctrl plus these characters send the byte; Ctrl plus a letter sends its alphabet index.
constexpr std::array<std::pair<std::string_view, std::uint8_t>, 7> CONTROL{{
    {"@ 2", 0}, {"[3", 0x1b}, {"\\4", 0x1c}, {"]5", 0x1d}, {"^6", 0x1e}, {"_-7", 0x1f}, {"?8", 0x7f},
}};

Bytes bytes_of(std::string_view s) { return {s.begin(), s.end()}; }

bool eq_ignore_ascii_case(std::string_view a, std::string_view b) {
    return std::ranges::equal(a, b, [](char x, char y) {
        return std::tolower(static_cast<unsigned char>(x)) == std::tolower(static_cast<unsigned char>(y));
    });
}

} // namespace

std::optional<KeyAction> key_action(const Key& key, Modifiers m, std::optional<std::string_view> typed, TermMode mode) {
#if defined(__APPLE__)
    const bool clip = m.logo() && !m.control();
#else
    const bool clip = m.control() && m.shift() && !m.alt();
#endif
    if (key.kind == Key::Kind::Character && clip && eq_ignore_ascii_case(key.text, "c")) return KeyAction::copy();
    if (key.kind == Key::Kind::Character && clip && eq_ignore_ascii_case(key.text, "v")) return KeyAction::paste();
    if (key.kind == Key::Kind::Named && m.shift() && !m.control() && !m.alt()) {
        if (auto action = shifted(key.name)) return action;
    }
    if (typed && typed->empty()) typed.reset();
    // The xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    const unsigned param = 1U + (m.shift() ? 1U : 0U) + (m.alt() ? 2U : 0U) + (m.control() ? 4U : 0U);
    const auto letter = [&](bool is_ss3, std::uint8_t x) {
        if (param == 1) return Bytes{0x1b, static_cast<std::uint8_t>(is_ss3 ? 'O' : '['), x};
        return bytes_of("\x1b[1;" + std::to_string(param) + static_cast<char>(x));
    };
    const auto alt = [&](Bytes b) {
        if (m.alt()) b.insert(b.begin(), 0x1b);
        return b;
    };
    Bytes out;
    switch (key.kind) {
    case Key::Kind::Named: {
        if (key.name == Named::Tab && m.shift()) {
            out = bytes_of("\x1b[Z");
            break;
        }
        const auto* it = std::ranges::find_if(NAMED, [&](const auto& e) { return e.first == key.name; });
        if (it == NAMED.end()) return std::nullopt;
        const Seq& seq = it->second;
        switch (seq.kind) {
        case Seq::Kind::Text: out = alt(bytes_of(m.control() ? seq.ctrl : seq.plain)); break;
        case Seq::Kind::Cursor: out = letter(mode.contains(TermMode::APP_CURSOR), seq.code); break;
        case Seq::Kind::Ss3: out = letter(true, seq.code); break;
        case Seq::Kind::Tilde:
            out = bytes_of(param == 1 ? "\x1b[" + std::to_string(seq.code) + "~"
                                      : "\x1b[" + std::to_string(seq.code) + ";" + std::to_string(param) + "~");
            break;
        }
        break;
    }
    case Key::Kind::Character: {
        std::optional<std::uint8_t> control;
        if (key.text.size() == 1) {
            const auto b = static_cast<std::uint8_t>(std::tolower(static_cast<unsigned char>(key.text[0])));
            if (b >= 'a' && b <= 'z') {
                control = static_cast<std::uint8_t>(b - 'a' + 1);
            } else {
                for (const auto& [keys, byte] : CONTROL)
                    if (keys.find(static_cast<char>(b)) != std::string_view::npos) {
                        control = byte;
                        break;
                    }
            }
        }
        out = alt(control && m.control() ? Bytes{*control} : bytes_of(typed ? *typed : std::string_view(key.text)));
        break;
    }
    case Key::Kind::Unidentified:
        if (!typed) return std::nullopt;
        out = alt(bytes_of(*typed));
        break;
    }
    return KeyAction::send(std::move(out));
}

Bytes encode_paste(std::string_view text, TermMode mode) {
    Bytes out;
    if (mode.contains(TermMode::BRACKETED_PASTE)) {
        out = bytes_of("\x1b[200~");
        for (const char c : text)
            if (c != '\x1b') out.push_back(static_cast<std::uint8_t>(c));
        const auto end = bytes_of("\x1b[201~");
        out.insert(out.end(), end.begin(), end.end());
        return out;
    }
    out.reserve(text.size());
    for (std::size_t i = 0; i < text.size(); ++i) {
        if (text[i] == '\r' && i + 1 < text.size() && text[i + 1] == '\n') {
            out.push_back('\r');
            ++i;
        } else {
            out.push_back(static_cast<std::uint8_t>(text[i] == '\n' ? '\r' : text[i]));
        }
    }
    return out;
}

} // namespace loa::ui::terminal
