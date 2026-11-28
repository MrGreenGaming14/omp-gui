#include "loa/ui/shell/keymap.hpp"

#include <algorithm>
#include <array>
#include <utility>

#include "unicode.hpp"

namespace loa::ui::shell::palette {

namespace {

constexpr std::array<std::string_view, 34> NAMED_NAMES = {
    "Escape", "Enter", "Tab", "Space", "Backspace", "Delete", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "PageUp", "PageDown",
    "Home", "End", "Insert", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    "Shift", "Control", "Alt", "AltGraph", "Super", "Meta", "Hyper",
};

struct ModifierLabel {
    Modifiers m;
    std::string_view word, glyph;
};
// In display order.
constexpr std::array<ModifierLabel, 4> MODIFIERS = {{
    {Modifiers::ctrl(), "Ctrl+", "\u2303"},
    {Modifiers::alt(), "Alt+", "\u2325"},
    {Modifiers::shift(), "Shift+", "\u21e7"},
    {Modifiers::logo(), "Super+", "\u2318"},
}};

struct NamedLabel {
    Named n;
    std::string_view word, glyph;
};
// Others use iced's name (`F5`, `Insert`).
constexpr std::array<NamedLabel, 14> NAMED = {{
    {Named::Escape, "Esc", "\u238b"}, {Named::Enter, "Enter", "\u21a9"}, {Named::Tab, "Tab", "\u21e5"},
    {Named::Space, "Space", "Space"}, {Named::Backspace, "Backspace", "\u232b"}, {Named::Delete, "Delete", "\u2326"},
    {Named::ArrowUp, "Up", "\u2191"}, {Named::ArrowDown, "Down", "\u2193"}, {Named::ArrowLeft, "Left", "\u2190"},
    {Named::ArrowRight, "Right", "\u2192"}, {Named::PageUp, "PageUp", "\u21de"}, {Named::PageDown, "PageDown", "\u21df"},
    {Named::Home, "Home", "\u2196"}, {Named::End, "End", "\u2198"},
}};

// Stands for the platform's primary modifier in `DEFAULTS`.
constexpr Modifiers P = Modifiers::logo();
constexpr Modifiers NONE = Modifiers::none();
constexpr Modifiers SHIFT = Modifiers::shift();
constexpr Modifiers CTRL = Modifiers::ctrl();
constexpr Modifiers ALT = Modifiers::alt();

struct Default {
    ChordKey key;
    Modifiers m;
    Scope scope;
    std::string_view id;
};

// The default bindings; Ctrl/Cmd+1…9 and Ctrl/Cmd+Alt+1…9 are added in a loop.
const std::array<Default, 17> DEFAULTS = {{
    {Named::Escape, NONE, Scope::Global, ids::SESSION_INTERRUPT},
    {U'k', P, Scope::Global, ids::PALETTE_TOGGLE},
    {U'k', P, Scope::Palette, ids::PALETTE_TOGGLE},
    {U'/', P, Scope::Global, ids::KEYMAP_SHOW},
    {U'b', P, Scope::Global, ids::PANEL_TOGGLE},
    {U'a', ALT, Scope::Global, ids::PANEL_AGENTS},
    {U'n', P, Scope::Global, ids::SESSION_NEW},
    {Named::Tab, CTRL, Scope::Global, ids::SESSION_NEXT},
    {Named::Tab, CTRL | SHIFT, Scope::Global, ids::SESSION_PREVIOUS},
    {Named::PageDown, P, Scope::Global, ids::PROJECT_TAB_NEXT},
    {Named::PageUp, P, Scope::Global, ids::PROJECT_TAB_PREVIOUS},
    {U'l', P, Scope::Global, ids::COMPOSER_FOCUS},
    {U'f', P | SHIFT, Scope::Global, ids::SEARCH_FOCUS},
    {Named::Tab, NONE, Scope::Global, ids::FOCUS_NEXT},
    {Named::Tab, SHIFT, Scope::Global, ids::FOCUS_PREVIOUS},
    {Named::Enter, NONE, Scope::Composer, ids::COMPOSER_SUBMIT},
    {Named::Enter, SHIFT, Scope::Composer, ids::COMPOSER_NEWLINE},
}};

bool is_modifier_key(Named n) { return n >= Named::Shift; }

} // namespace

std::string_view name(Named n) { return NAMED_NAMES[static_cast<std::size_t>(n)]; }

Chord Chord::make(ChordKey key, Modifiers modifiers) {
    if (const auto* c = std::get_if<char32_t>(&key)) key = detail::to_lower(*c);
    return Chord{key, modifiers & (Modifiers::ctrl() | Modifiers::alt() | Modifiers::shift() | Modifiers::logo())};
}

std::optional<Chord> Chord::from_event(const KeyPress& event) {
    ChordKey k;
    if (const auto* n = std::get_if<Named>(&event.key)) {
        if (is_modifier_key(*n)) return std::nullopt;
        k = *n;
    } else if (const auto* text = std::get_if<std::string>(&event.key)) {
        if (event.latin) {
            k = *event.latin;
        } else {
            const auto first = detail::first_char(*text);
            if (!first) return std::nullopt;
            k = *first;
        }
    } else {
        return std::nullopt;
    }
    return make(k, event.modifiers);
}

std::string Chord::label(Platform platform) const {
    const auto pick = [&](std::string_view word, std::string_view glyph) { return platform == Platform::MacOs ? glyph : word; };
    std::string out;
    for (const auto& m : MODIFIERS)
        if (modifiers.contains(m.m)) out += pick(m.word, m.glyph);
    if (const auto* c = std::get_if<char32_t>(&key)) {
        detail::push_utf8(out, detail::to_upper(*c));
    } else {
        const auto n = std::get<Named>(key);
        const auto it = std::ranges::find(NAMED, n, &NamedLabel::n);
        out += it != NAMED.end() ? pick(it->word, it->glyph) : name(n);
    }
    return out;
}

Keymap Keymap::defaults(Platform platform) {
    const auto p = primary(platform);
    Keymap map;
    map.platform_ = platform;
    const auto bind = [&](ChordKey key, Modifiers m, Scope scope, CommandId id) {
        map.bindings_.emplace_back(Chord::make(key, m.contains(P) ? (m.difference(P) | p) : m), scope, std::move(id));
    };
    for (const auto& d : DEFAULTS) {
        bind(d.key, d.m, d.scope, CommandId(d.id));
    }
    for (std::size_t n = 1; n <= 9; ++n) {
        const auto digit = static_cast<char32_t>(U'0' + n);
        bind(digit, P, Scope::Global, ids::session_goto(n));
        bind(digit, P | ALT, Scope::Global, ids::panel_tab(n));
    }
    return map;
}

const CommandId* Keymap::resolve(const KeyPress& event, Scope scope) const {
    if (event.repeat) return nullptr;
    const auto chord = Chord::from_event(event);
    if (!chord) return nullptr;
    const auto find = [&](Scope s) -> const CommandId* {
        for (const auto& [c, bound, id] : bindings_)
            if (bound == s && c == *chord) return &id;
        return nullptr;
    };
    if (const auto* id = find(scope)) return id;
    return scope == Scope::Composer ? find(Scope::Global) : nullptr;
}

std::optional<Chord> Keymap::chord_for(const CommandId& command) const {
    const Binding* first = nullptr;
    for (const auto& b : bindings_) {
        if (std::get<2>(b) != command) continue;
        if (std::get<1>(b) == Scope::Global) return std::get<0>(b);
        if (!first) first = &b;
    }
    if (!first) return std::nullopt;
    return std::get<0>(*first);
}

} // namespace loa::ui::shell::palette
