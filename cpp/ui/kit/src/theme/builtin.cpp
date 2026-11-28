// The shipped themes: warm neutrals with one sage accent, dark and light, defined once.
// The only file (with tokens.cpp's hex parser) allowed colour literals; the lint skips it.

#include "builtin.hpp"

#include <array>

namespace loa::ui::kit::theme {
namespace {

using C = ColorRole;
constexpr Color c(std::uint32_t rgb, float a = 1.0F) { return Color::hex(rgb, a); }

// Colours in `ColorRole` order; the terminal colours are the ANSI 16.
constexpr std::array<Color, COUNT<ColorRole>> DARK = {
    c(0x262624), c(0x1f1e1d), c(0x2b2a28), c(0x30302e), c(0x2b2a28), c(0x34332f),
    c(0x33322f), c(0x4a4945), c(0xecebe6), c(0xb7b5ac), c(0x9a988f), c(0x7fb7a4),
    c(0x1f1e1d), c(0x6fbf73), c(0xf0b866), c(0xe8a0a0), c(0x6fbf73, 0.14F), c(0xe8a0a0, 0.14F),
    c(0x7fb7a4), Color{0.055F, 0.051F, 0.047F, 0.6F}, Color{0.0F, 0.0F, 0.0F, 0.45F},
    c(0x3a3a37), c(0xe8a0a0), c(0x6fbf73), c(0xf0b866), c(0x6fa0bf), c(0xaca0b0),
    c(0x6fb099), c(0xbabab6), c(0x7f7f7b), c(0xe9b6b5), c(0x94cc95), c(0xefc78c),
    c(0x94b7cb), c(0xbfb7c0), c(0x94c1b0), c(0xecebe6),
};

constexpr std::array<Color, COUNT<ColorRole>> LIGHT = {
    c(0xfaf9f5), c(0xf1efe8), c(0xffffff), c(0xffffff), c(0xebe8df), c(0xe4e0d5),
    c(0xe3e0d6), c(0xcbc7bb), c(0x1f1e1d), c(0x55534d), c(0x6f6d66), c(0x2f7a66),
    c(0xffffff), c(0x2f8f5b), c(0x8a560c), c(0xb3423a), c(0x2f8f5b, 0.12F), c(0xb3423a, 0.12F),
    c(0x2f7a66), Color{0.1F, 0.1F, 0.09F, 0.35F}, Color{0.0F, 0.0F, 0.0F, 0.18F},
    c(0x353433), c(0xb3423a), c(0x2f8f5b), c(0x8a560c), c(0x2f438f), c(0x714364),
    c(0x2f6975), c(0xc3c2bf), c(0x82817e), c(0xc87972), c(0x6caf89), c(0xac8752),
    c(0x6c7aae), c(0x9a7990), c(0x6c949b), c(0xfaf9f5),
};

std::optional<StyleSpec> spec(std::optional<Paint> bg, std::optional<C> fg, std::optional<Paint> border,
                              std::optional<RadiusRole> radius) {
    StyleSpec s;
    s.bg = bg;
    if (fg) s.fg = Paint{Paint::RoleRef{*fg}};
    if (border) s.border = std::pair{*border, 1.0F};
    s.radius = radius;
    return s;
}

Paint role(C r) { return Paint{Paint::RoleRef{r}}; }
Paint alpha(C r, AlphaRole a) { return Paint{Paint::Alpha{r, a}}; }

using States = std::array<std::optional<StyleSpec>, COUNT<StyleState>>;

// The default recipes; overrides and scripts edit these by `component.part.state.field`.
std::map<StyleKey, StyleStates> styles() {
    using enum RadiusRole;
    auto filled = [](C bg, C fg, RadiusRole r) {
        States s{};
        s[0] = spec(role(bg), fg, std::nullopt, r);
        s[index(StyleState::Hover)] = spec(alpha(bg, AlphaRole::Hover), std::nullopt, std::nullopt, std::nullopt);
        s[index(StyleState::Disabled)] = spec(alpha(bg, AlphaRole::Disabled), std::nullopt, std::nullopt, std::nullopt);
        return s;
    };
    auto hovered = [](auto base, auto hover, auto selected) { return States{base, hover, std::nullopt, selected, std::nullopt}; };
    auto plain = [](std::optional<C> bg, std::optional<C> fg, std::optional<Paint> border, std::optional<RadiusRole> r) {
        return States{spec(bg ? std::optional(role(*bg)) : std::nullopt, fg, border, r), std::nullopt, std::nullopt,
                      std::nullopt, std::nullopt};
    };
    const auto hover = spec(role(C::Hover), C::Text, std::nullopt, std::nullopt);
    const auto selected = spec(role(C::Selected), C::Text, std::nullopt, std::nullopt);
    const auto none = std::optional<StyleSpec>{};
    const std::optional<C> no_c;
    const std::optional<Paint> no_p;

    const std::pair<StyleKey, States> table[] = {
        {{"button", "primary"}, filled(C::Text, C::Side, Row)},
        {{"button", "accent"}, filled(C::Accent, C::OnAccent, Row)},
        {{"button", "danger"}, filled(C::Danger, C::OnAccent, Row)},
        {{"button", "secondary"}, hovered(spec(std::nullopt, C::Text, role(C::LineStrong), Row), hover, none)},
        {{"button", "ghost"}, hovered(spec(std::nullopt, C::Text2, std::nullopt, Chip), hover, none)},
        {{"button", "row"}, States{spec(std::nullopt, C::Text, std::nullopt, Row), hover, selected, selected, std::nullopt}},
        {{"button", "tab"}, hovered(spec(std::nullopt, C::Text2, std::nullopt, Row), hover, selected)},
        {{"button", "flat"}, plain(no_c, C::Text, no_p, Chip)},
        {{"container", "ground"}, plain(C::Ground, C::Text, no_p, std::nullopt)},
        {{"container", "side"}, plain(C::Side, C::Text, no_p, std::nullopt)},
        {{"container", "card"}, plain(C::Card, C::Text, role(C::Line), Card)},
        {{"container", "attention"}, plain(C::Card, C::Text, alpha(C::Warn, AlphaRole::Tint), Card)},
        {{"container", "raised"}, plain(C::Raised, C::Text, role(C::Line), Card)},
        {{"container", "popup"}, plain(C::Card, C::Text, role(C::LineStrong), Card)},
        {{"container", "tooltip"}, plain(C::Raised, C::Text, role(C::LineStrong), Chip)},
        {{"container", "well"}, plain(C::Ground, C::Text, role(C::LineStrong), Chip)},
        {{"container", "error"},
         States{spec(alpha(C::Danger, AlphaRole::Tint), C::Text, role(C::Danger), Chip), std::nullopt, std::nullopt,
                std::nullopt, std::nullopt}},
        {{"container", "badge"}, plain(no_c, C::Text3, no_p, std::nullopt)},
        {{"container", "scrim"}, plain(C::Scrim, no_c, no_p, std::nullopt)},
    };
    std::map<StyleKey, StyleStates> out;
    for (const auto& [k, s] : table) out.emplace(k, StyleStates{s});
    return out;
}

TokenSet make(Variant variant) {
    TokenSet t;
    t.name = variant == Variant::Dark ? "Loa Dark" : "Loa Light";
    t.variant = variant;
    t.colors = variant == Variant::Dark ? DARK : LIGHT;
    const float text[] = {20, 16, 14, 13, 12, 12};
    for (std::size_t i = 0; i < t.text.size(); ++i) t.text[i] = {text[i], Font::Default};
    t.text[index(TextRole::Mono)].second = Font::Monospace;
    t.space = {4, 8, 12, 16, 24, 24};
    t.radii = {4, 6, 8, 12, 99};
    t.alphas = {0.55F, 0.4F, 0.35F, 0.9F};
    t.layout = {700, 272, 380, 520, 760, 160, 190, 1440, 900, 900, 640, 660, 640, 560, 780, 360, 280};
    t.styles = styles();
    return t;
}

constexpr Variant SYSTEM_VARIANTS[] = {Variant::Dark, Variant::Light};
constexpr Variant DARK_VARIANTS[] = {Variant::Dark};
constexpr Variant LIGHT_VARIANTS[] = {Variant::Light};

} // namespace

std::shared_ptr<const TokenSet> builtin_set(Variant v) {
    static const std::array<std::shared_ptr<const TokenSet>, 2> sets = {
        std::make_shared<const TokenSet>(make(Variant::Light)), std::make_shared<const TokenSet>(make(Variant::Dark))};
    return sets[index(v)];
}

std::span<const BuiltinTheme> BuiltinTheme::all() {
    static const std::array<BuiltinTheme, 3> themes = {
        BuiltinTheme{"system", "System", SYSTEM_VARIANTS},
        BuiltinTheme{"dark", "Dark", DARK_VARIANTS},
        BuiltinTheme{"light", "Light", LIGHT_VARIANTS},
    };
    return themes;
}

std::expected<TokenSet, ThemeError> BuiltinTheme::tokens(Variant v) const {
    for (const Variant offered : variants_)
        if (offered == v) return *builtin_set(v);
    return std::unexpected(ThemeError::no_variant(std::string(id_), v));
}

} // namespace loa::ui::kit::theme
