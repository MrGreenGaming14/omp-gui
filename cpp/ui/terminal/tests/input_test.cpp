// xterm key encodings and paste (the Rust `keys_encode_per_xterm`, widened to arrows, function keys and Ctrl letters).

#include <doctest/doctest.h>

#include <string>

#include "loa/ui/terminal/input.hpp"

using namespace loa::ui::terminal;

namespace {
Bytes b(std::string_view s) { return {s.begin(), s.end()}; }
std::optional<KeyAction> send(std::string_view s) { return KeyAction::send(b(s)); }
constexpr Modifiers NO{}, CTRL{Modifiers::CTRL}, SHIFT{Modifiers::SHIFT}, ALT{Modifiers::ALT};
constexpr TermMode NONE{}, APP{TermMode::APP_CURSOR}, BRACKETED{TermMode::BRACKETED_PASTE};
#if defined(__APPLE__)
constexpr Modifiers CLIP{Modifiers::LOGO};
#else
constexpr Modifiers CLIP{Modifiers::CTRL | Modifiers::SHIFT};
#endif

struct Case {
    Key key;
    Modifiers m;
    std::optional<std::string_view> text;
    TermMode mode;
    std::optional<KeyAction> want;
};
} // namespace

TEST_CASE("keys encode per xterm") {
    using namespace std::string_view_literals;
    const Case cases[] = {
        // The Rust table.
        {Key::character("c"), CTRL, std::nullopt, NONE, send("\x03")},
        {Key::character("["), CTRL, std::nullopt, NONE, send("\x1b")},
        {Key::character("b"), ALT, "b", NONE, send("\x1b" "b")},
        {Key::character("\xc3\xa9"), NO, "\xc3\xa9", NONE, send("\xc3\xa9")},
        {Key::named(Named::Backspace), NO, std::nullopt, NONE, send("\x7f")},
        {Key::named(Named::Tab), SHIFT, std::nullopt, NONE, send("\x1b[Z")},
        {Key::named(Named::ArrowUp), NO, std::nullopt, APP, send("\x1bOA")},
        {Key::named(Named::ArrowUp), CTRL, std::nullopt, APP, send("\x1b[1;5A")},
        {Key::named(Named::F5), SHIFT, std::nullopt, NONE, send("\x1b[15;2~")},
        {Key::character("C"), CLIP, std::nullopt, NONE, KeyAction::copy()},
        {Key::named(Named::PageUp), SHIFT, std::nullopt, NONE, KeyAction::scrolled(Scroll::page_up())},
        // Arrows, Home/End.
        {Key::named(Named::ArrowUp), NO, std::nullopt, NONE, send("\x1b[A")},
        {Key::named(Named::ArrowDown), NO, std::nullopt, NONE, send("\x1b[B")},
        {Key::named(Named::ArrowRight), NO, std::nullopt, NONE, send("\x1b[C")},
        {Key::named(Named::ArrowLeft), NO, std::nullopt, NONE, send("\x1b[D")},
        {Key::named(Named::ArrowLeft), NO, std::nullopt, APP, send("\x1bOD")},
        {Key::named(Named::ArrowRight), SHIFT, std::nullopt, NONE, send("\x1b[1;2C")},
        {Key::named(Named::ArrowDown), ALT, std::nullopt, NONE, send("\x1b[1;3B")},
        {Key::named(Named::Home), NO, std::nullopt, NONE, send("\x1b[H")},
        {Key::named(Named::End), NO, std::nullopt, APP, send("\x1bOF")},
        // Function keys.
        {Key::named(Named::F1), NO, std::nullopt, NONE, send("\x1bOP")},
        {Key::named(Named::F4), NO, std::nullopt, APP, send("\x1bOS")},
        {Key::named(Named::F2), CTRL, std::nullopt, NONE, send("\x1b[1;5Q")},
        {Key::named(Named::F5), NO, std::nullopt, NONE, send("\x1b[15~")},
        {Key::named(Named::F6), NO, std::nullopt, NONE, send("\x1b[17~")},
        {Key::named(Named::F10), NO, std::nullopt, NONE, send("\x1b[21~")},
        {Key::named(Named::F11), NO, std::nullopt, NONE, send("\x1b[23~")},
        {Key::named(Named::F12), CTRL, std::nullopt, NONE, send("\x1b[24;5~")},
        {Key::named(Named::Insert), NO, std::nullopt, NONE, send("\x1b[2~")},
        {Key::named(Named::Delete), NO, std::nullopt, NONE, send("\x1b[3~")},
        {Key::named(Named::PageDown), NO, std::nullopt, NONE, send("\x1b[6~")},
        // Ctrl letters and punctuation.
        {Key::character("a"), CTRL, "a", NONE, send("\x01")},
        {Key::character("z"), CTRL, std::nullopt, NONE, send("\x1a")},
        {Key::character("D"), CTRL, std::nullopt, NONE, send("\x04")},
        {Key::character("x"), Modifiers{Modifiers::CTRL | Modifiers::ALT}, std::nullopt, NONE, send("\x1b\x18")},
        {Key::character("2"), CTRL, std::nullopt, NONE, send("\0"sv)},
        {Key::character("\\"), CTRL, std::nullopt, NONE, send("\x1c")},
        {Key::character("-"), CTRL, std::nullopt, NONE, send("\x1f")},
        {Key::character("?"), CTRL, std::nullopt, NONE, send("\x7f")},
        // Plain text and named text keys.
        {Key::character("l"), NO, "l", NONE, send("l")},
        {Key::character("A"), SHIFT, "A", NONE, send("A")},
        {Key::named(Named::Enter), NO, std::nullopt, NONE, send("\r")},
        {Key::named(Named::Backspace), CTRL, std::nullopt, NONE, send("\x08")},
        {Key::named(Named::Escape), ALT, std::nullopt, NONE, send("\x1b\x1b")},
        {Key::named(Named::Space), CTRL, std::nullopt, NONE, send("\0"sv)},
        {Key::named(Named::Tab), NO, std::nullopt, NONE, send("\t")},
        {Key::unidentified(), NO, "\xe2\x82\xac", NONE, send("\xe2\x82\xac")},
        {Key::unidentified(), NO, std::nullopt, NONE, std::nullopt},
        {Key::named(Named::Other), NO, std::nullopt, NONE, std::nullopt},
        // Pane shortcuts.
        {Key::character("v"), CLIP, std::nullopt, NONE, KeyAction::paste()},
        {Key::named(Named::Insert), SHIFT, std::nullopt, NONE, KeyAction::paste()},
        {Key::named(Named::PageDown), SHIFT, std::nullopt, NONE, KeyAction::scrolled(Scroll::page_down())},
        {Key::named(Named::Home), SHIFT, std::nullopt, NONE, KeyAction::scrolled(Scroll::top())},
        {Key::named(Named::End), SHIFT, std::nullopt, NONE, KeyAction::scrolled(Scroll::bottom())},
    };
    for (const auto& c : cases) {
        CAPTURE(c.key.text);
        CAPTURE(static_cast<int>(c.key.name));
        CHECK(key_action(c.key, c.m, c.text, c.mode) == c.want);
    }
}

TEST_CASE("pastes are bracketed or turn line feeds into returns") {
    CHECK(encode_paste("make\x1b\ntest", BRACKETED) == b("\x1b[200~make\ntest\x1b[201~"));
    CHECK(encode_paste("a\nb\r\nc", NONE) == b("a\rb\rc"));
    CHECK(encode_paste("a\rb", NONE) == b("a\rb"));
    CHECK(encode_paste("", BRACKETED) == b("\x1b[200~\x1b[201~"));
}
