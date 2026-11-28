// Port of the tests in `loa-ui-shell/src/palette/keymap.rs`.

#include <doctest/doctest.h>

#include "loa/ui/shell/keymap.hpp"

using namespace loa::ui::shell::palette;

namespace {
KeyPress ch(std::string text, char32_t latin, Modifiers m, bool repeat = false) { return KeyPress{std::move(text), latin, m, repeat}; }
KeyPress named(Named n, Modifiers m) { return KeyPress{n, std::nullopt, m, false}; }
CommandId id(std::string_view s) { return CommandId(s); }
const auto CTRL = Modifiers::ctrl();
const auto SHIFT = Modifiers::shift();
const auto ALT = Modifiers::alt();
const auto LOGO = Modifiers::logo();
const auto NONE = Modifiers::none();
} // namespace

TEST_CASE("defaults resolve per platform and scope") {
    const auto mac = Keymap::defaults(Platform::MacOs);
    const auto other = Keymap::defaults(Platform::Other);
    struct Case {
        const Keymap* map;
        KeyPress event;
        Scope scope;
        std::optional<std::string_view> want;
    };
    const Case cases[] = {
        {&mac, ch("k", U'k', LOGO), Scope::Global, ids::PALETTE_TOGGLE},
        {&mac, ch("k", U'k', CTRL), Scope::Global, std::nullopt},
        {&other, ch("k", U'k', CTRL), Scope::Global, ids::PALETTE_TOGGLE},
        {&other, ch("b", U'b', CTRL), Scope::Composer, ids::PANEL_TOGGLE},
        {&other, ch("a", U'a', ALT), Scope::Global, ids::PANEL_AGENTS},
        {&other, ch("a", U'a', ALT), Scope::Composer, ids::PANEL_AGENTS},
        {&mac, ch("a", U'a', ALT), Scope::Global, ids::PANEL_AGENTS},
        {&other, ch("b", U'b', CTRL), Scope::Palette, std::nullopt},
        {&other, ch("k", U'k', CTRL), Scope::Palette, ids::PALETTE_TOGGLE},
        {&other, ch("K", U'k', CTRL | SHIFT), Scope::Global, std::nullopt},
        {&other, ch("\u043b", U'k', CTRL), Scope::Global, ids::PALETTE_TOGGLE},
        {&other, named(Named::Enter, NONE), Scope::Global, std::nullopt},
        {&other, named(Named::Enter, NONE), Scope::Composer, ids::COMPOSER_SUBMIT},
        {&other, named(Named::Enter, SHIFT), Scope::Composer, ids::COMPOSER_NEWLINE},
        {&other, named(Named::Escape, NONE), Scope::Composer, ids::SESSION_INTERRUPT},
        {&mac, named(Named::PageDown, LOGO), Scope::Composer, ids::PROJECT_TAB_NEXT},
        {&other, named(Named::Control, CTRL), Scope::Global, std::nullopt},
        {&other, ch("k", U'k', CTRL, true), Scope::Global, std::nullopt},
    };
    int i = 0;
    for (const auto& c : cases) {
        CAPTURE(i);
        const auto* got = c.map->resolve(c.event, c.scope);
        if (c.want) {
            REQUIRE(got != nullptr);
            CHECK(got->as_str() == *c.want);
        } else {
            CHECK(got == nullptr);
        }
        ++i;
    }
    // Without a physical mapping the typed character counts.
    CHECK(other.resolve(KeyPress{std::string("k"), std::nullopt, CTRL, false}, Scope::Global)->as_str() == ids::PALETTE_TOGGLE);
}

TEST_CASE("defaults bind each chord once and label per platform") {
    const auto mac = Keymap::defaults(Platform::MacOs);
    const auto other = Keymap::defaults(Platform::Other);
    for (const auto* map : {&mac, &other}) {
        const auto& b = map->bindings();
        for (std::size_t i = 0; i < b.size(); ++i)
            for (std::size_t j = i + 1; j < b.size(); ++j)
                CHECK_FALSE((std::get<0>(b[i]) == std::get<0>(b[j]) && std::get<1>(b[i]) == std::get<1>(b[j])));
    }
    const auto hint = [](const Keymap& map, std::string_view command) { return map.chord_for(id(command))->label(map.platform()); };
    CHECK(hint(mac, ids::PALETTE_TOGGLE) == "\u2318K");
    CHECK(hint(other, ids::PALETTE_TOGGLE) == "Ctrl+K");
    CHECK(hint(other, ids::SESSION_INTERRUPT) == "Esc");
    CHECK(hint(mac, ids::COMPOSER_NEWLINE) == "\u21e7\u21a9");
    CHECK(hint(other, ids::COMPOSER_NEWLINE) == "Shift+Enter");
    CHECK(hint(other, ids::PROJECT_TAB_NEXT) == "Ctrl+PageDown");
    CHECK(Chord::character(U'p', CTRL | SHIFT | LOGO).label(Platform::MacOs) == "\u2303\u21e7\u2318P");
    CHECK(Chord::named(Named::F5, ALT).label(Platform::Other) == "Alt+F5");
}

TEST_CASE("every primary binding uses Cmd on macOS and Ctrl elsewhere") {
    const auto mac = Keymap::defaults(Platform::MacOs);
    const auto other = Keymap::defaults(Platform::Other);
    REQUIRE(mac.bindings().size() == other.bindings().size());
    for (std::size_t i = 0; i < mac.bindings().size(); ++i) {
        const auto& [mc, ms, mid] = mac.bindings()[i];
        const auto& [lc, ls, lid] = other.bindings()[i];
        CHECK(mid == lid);
        CHECK(ms == ls);
        CHECK(mc.key == lc.key);
        if (mc.modifiers.contains(LOGO)) {
            CHECK(lc.modifiers == (mc.modifiers.difference(LOGO) | CTRL));
        } else {
            CHECK(mc.modifiers == lc.modifiers);
        }
    }
    CHECK(mac.chord_for(id(ids::SESSION_NEW))->modifiers == LOGO);
    CHECK(other.chord_for(id(ids::SESSION_NEW))->modifiers == CTRL);
    // Ctrl+Tab cycles sessions on both platforms.
    CHECK(mac.chord_for(id(ids::SESSION_NEXT))->modifiers == CTRL);
}

TEST_CASE("goto and panel tab ids match the Rust strings") {
    CHECK(ids::session_goto(1).as_str() == "session.goto.1");
    CHECK(ids::panel_tab(9).as_str() == "panel.tab.9");
    const auto other = Keymap::defaults(Platform::Other);
    CHECK(other.resolve(ch("3", U'3', CTRL), Scope::Global)->as_str() == "session.goto.3");
    CHECK(other.resolve(ch("3", U'3', CTRL | ALT), Scope::Global)->as_str() == "panel.tab.3");
    CHECK(ids::SEARCH_FOCUS == "sidebar.search");
    CHECK(ids::PROJECT_TAB_PREVIOUS == "project.tab.previous");
}
