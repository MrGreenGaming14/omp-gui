// Port of `loa-ui-kit/src/theme/tests.rs`.

#include <doctest/doctest.h>

#include <algorithm>
#include <string>

#include <toml++/toml.hpp>

#include "loa/ui/kit/style.hpp"
#include "loa/ui/kit/theme.hpp"

using namespace loa::ui::kit;
using namespace loa::ui::kit::theme;

namespace {

TokenSet builtin(Variant v) {
    auto s = ThemeRegistry::builtin().resolve(ThemeRef{std::string(name(v))});
    REQUIRE(s.has_value());
    auto t = (*s)->tokens(v);
    REQUIRE(t.has_value());
    return *t;
}

template <Role R> void round_trip() {
    for (const R r : all<R>()) CHECK(parse<R>(name(r)) == std::optional<R>(r));
    CHECK(all<R>().size() == COUNT<R>);
}

toml::table parse_toml(std::string_view src) {
#if TOML_EXCEPTIONS
    return toml::parse(src);
#else
    auto r = toml::parse(src);
    REQUIRE(r.succeeded());
    return std::move(r).table();
#endif
}

Appearance appearance(std::string name) { return Appearance{ThemeRef{std::move(name)}, {}}; }

} // namespace

TEST_CASE("text roles meet WCAG AA on every ground of both variants") {
    using enum ColorRole;
    const std::pair<ColorRole, ColorRole> pairs[] = {
        {Text, Ground}, {Text2, Ground}, {Text3, Ground}, {Warn, Ground}, {Text, Side}, {Text2, Side}, {Text3, Side},
        {Warn, Side},   {Text, Card},    {Text2, Card},   {Text3, Card},  {OnAccent, Accent}, {Side, Text},
    };
    for (const Variant v : all<Variant>()) {
        const auto t = builtin(v);
        for (const auto& [fg, bg] : pairs) {
            const float c = contrast(t.color(fg), t.color(bg));
            INFO(name(v), ": ", name(fg), " on ", name(bg), " is ", c);
            CHECK(c >= 4.5F);
        }
    }
}

TEST_CASE("role names round trip and index their tables") {
    round_trip<ColorRole>();
    round_trip<TextRole>();
    round_trip<SpaceRole>();
    round_trip<RadiusRole>();
    round_trip<AlphaRole>();
    round_trip<LayoutRole>();
    CHECK(RoleNames<ColorRole>::KIND == "color");
    CHECK(name(ColorRole::LineStrong) == "line.strong");
    CHECK(name(LayoutRole::MinHeight) == "min.height");
    CHECK(term(0) == ColorRole::Term0);
    CHECK(term(15) == ColorRole::Term15);
    CHECK(term(99) == ColorRole::Term15);
    const auto t = builtin(Variant::Dark);
    CHECK(t.palette().background == t.color(ColorRole::Ground));
    CHECK(t.font(TextRole::Mono) == Font::Monospace);
    CHECK(t.font(TextRole::Body) == Font::Default);
    // The hex values of `builtin.rs` survive the port.
    CHECK(t.color(ColorRole::Ground) == Color::rgb8(0x26, 0x26, 0x24));
    CHECK(builtin(Variant::Light).color(ColorRole::Accent) == Color::rgb8(0x2f, 0x7a, 0x66));
    CHECK(t.color(ColorRole::AddedBg) == Color::hex(0x6fbf73, 0.14F));
}

TEST_CASE("overrides parse, apply and report failures") {
    auto t = builtin(Variant::Dark);
    const auto table = parse_toml(R"##(
        "color.accent" = "#ff000080"
        color.line = "text.2"
        color.hover = "accent @ 0.5"
        space.m = 20
        text.body = 15.5
        style.button.ghost.hover.bg = "danger @ tint"
        style.button.ghost.base.radius = "pill"
        color.nope = "#000000"
        space.m2 = 1
        layout.nav = "wide"
        style.button.ghost.hover.bg2 = "accent"
        flag = true
    )##");
    std::vector<std::string> errors;
    for (const auto& e : t.apply_table(table)) errors.push_back(e.to_string());
    CHECK(t.color(ColorRole::Accent) == Color::rgba8(255, 0, 0, 128.0F / 255.0F));
    CHECK(t.color(ColorRole::Line) == t.color(ColorRole::Text2));
    CHECK(t.color(ColorRole::Hover) == t.color(ColorRole::Accent).scale_alpha(0.5F));
    CHECK(t.spacing(SpaceRole::M) == 20.0F);
    CHECK(t.text_size(TextRole::Body) == 15.5F);
    const auto ghost = t.look(ButtonKind::ghost().key(), StyleState::Hover);
    CHECK(ghost.bg == std::optional(t.color(ColorRole::Danger).scale_alpha(0.35F)));
    CHECK(ghost.border.radius == t.radius(RadiusRole::Pill));
    CHECK(errors.size() == 5);
    auto any = [&](std::string_view s) { return std::ranges::any_of(errors, [s](const auto& e) { return e.find(s) != std::string::npos; }); };
    CHECK(any("color.nope"));
    CHECK(any("flag"));
    CHECK(any("unknown theme key `space.m2`"));
    CHECK(any("unknown theme key `layout.nav`"));
    CHECK(any("bad value for `style.button.ghost.hover.bg2`: unknown recipe or value"));
    CHECK(any("bad value for `flag`: true"));
}

TEST_CASE("token values parse like the Rust override parser") {
    CHECK(TokenValue::parse(" #123456 ") == TokenValue{Color::rgb8(0x12, 0x34, 0x56)});
    CHECK(TokenValue::parse("12.5") == TokenValue{12.5F});
    CHECK(TokenValue::parse("+3") == TokenValue{3.0F});
    CHECK(TokenValue::parse("accent") == TokenValue{TokenValue::Ref{"accent"}});
    CHECK(TokenValue::parse("text @ muted") == TokenValue{TokenValue::Alpha{"text", "muted"}});
    CHECK(TokenValue::parse("#12345") == TokenValue{TokenValue::Ref{"#12345"}});
    auto t = builtin(Variant::Light);
    CHECK(t.apply("space.m", TokenValue::parse("accent")).error() == ThemeError::bad_value("space.m", "expected a number"));
    CHECK(t.apply("color.text", TokenValue::parse("3")).error() == ThemeError::bad_value("color.text", "expected a colour"));
    CHECK(t.apply("nodot", TokenValue::parse("3")).error() == ThemeError::unknown_key("nodot"));
    CHECK(t.apply("style.button.ghost.base.border", TokenValue::parse("accent")).has_value());
    CHECK(t.look(ButtonKind::ghost().key(), StyleState::Base).border == Border{t.color(ColorRole::Accent), 1.0F, t.radius(RadiusRole::Chip)});
    CHECK(ThemeError::no_variant("dark", Variant::Light).to_string() == "theme `dark` does not offer the light variant");
}

TEST_CASE("registry resolves builtins, lists them and falls back") {
    const auto r = ThemeRegistry::builtin();
    std::vector<std::string> ids;
    for (const auto& [id, label] : r.list()) ids.push_back(id);
    CHECK(ids == std::vector<std::string>{"dark", "light", "system"});
    CHECK(r.tokens(appearance("light"), Variant::Dark).first.variant == Variant::Light);
    CHECK(r.tokens(appearance("system"), Variant::Light).first.variant == Variant::Light);
    const auto [set, errors] = r.tokens(appearance("neon"), Variant::Dark);
    CHECK(set.variant == Variant::Dark);
    CHECK(errors == std::vector<ThemeError>{ThemeError::unknown_theme("neon")});
    CHECK(errors[0].to_string() == "unknown theme `neon`");
}

TEST_CASE("recipes follow the active set and states") {
    auto t = builtin(Variant::Light);
    REQUIRE(t.apply("color.selected", TokenValue::parse("#123456")).has_value());
    const auto before = generation();
    CHECK(set_active(t) > before);
    CHECK(tokens()->variant == Variant::Light);
    const auto row = button(ButtonKind::row(true), ButtonStatus::Active);
    CHECK(row.background == std::optional(Color::rgb8(0x12, 0x34, 0x56)));
    const auto ghost = [](ButtonStatus s) { return button(ButtonKind::ghost(), s); };
    CHECK(ghost(ButtonStatus::Active).background == std::nullopt);
    CHECK(ghost(ButtonStatus::Active).text_color == t.color(ColorRole::Text2));
    CHECK(ghost(ButtonStatus::Pressed).background == std::optional(t.color(ColorRole::Hover)));
    CHECK(container(Surface::Popup).shadow.blur == 40.0F);
    CHECK(color(Tone::Muted) == t.color(ColorRole::Text2));
    // The builtin sets are untouched by the active set.
    CHECK(builtin_tokens(Variant::Dark)->color(ColorRole::Text) == builtin(Variant::Dark).color(ColorRole::Text));
    CHECK(builtin_tokens(Variant::Light)->color(ColorRole::Selected) != t.color(ColorRole::Selected));
    set_active(*builtin_tokens(Variant::Dark));
}
