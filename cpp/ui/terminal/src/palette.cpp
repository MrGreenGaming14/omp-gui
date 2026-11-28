#include "loa/ui/terminal/palette.hpp"

#include <algorithm>
#include <cmath>

namespace loa::ui::terminal {

namespace kt = loa::ui::kit::theme;

namespace {

float to_linear(float c) { return c <= 0.04045F ? c / 12.92F : std::pow((c + 0.055F) / 1.055F, 2.4F); }
float to_srgb(float c) { return c <= 0.0031308F ? c * 12.92F : 1.055F * std::pow(c, 1.0F / 2.4F) - 0.055F; }
Color rgb(Rgb c) { return Color::rgb8(c.r, c.g, c.b); }

} // namespace

Color mix(Color a, Color b, float factor) {
    const float t = std::clamp(factor, 0.0F, 1.0F);
    const auto ch = [t](float x, float y) { return to_srgb(to_linear(x) * (1 - t) + to_linear(y) * t); };
    return {ch(a.r, b.r), ch(a.g, b.g), ch(a.b, b.b), a.a * (1 - t) + b.a * t};
}

TermPalette TermPalette::from_tokens(const kt::TokenSet& t) {
    TermPalette p;
    for (std::size_t n = 0; n < p.ansi.size(); ++n) p.ansi[n] = t.color(kt::term(n));
    p.foreground = t.color(kt::ColorRole::Text);
    p.background = t.color(kt::ColorRole::Ground);
    p.cursor = t.color(kt::ColorRole::CursorRing);
    p.selection = t.color(kt::ColorRole::Accent).scale_alpha(t.alpha(kt::AlphaRole::Tint));
    return p;
}

Color TermPalette::resolve(CellColor color, const Colors& overrides) const {
    switch (color.kind) {
    case CellColor::Kind::Rgb: return rgb(color.rgb);
    case CellColor::Kind::Indexed:
        if (const auto& o = overrides[color.index]) return rgb(*o);
        return indexed(color.index);
    case CellColor::Kind::Foreground:
        if (const auto& o = overrides[COLOR_FOREGROUND]) return rgb(*o);
        return foreground;
    case CellColor::Kind::Background:
        if (const auto& o = overrides[COLOR_BACKGROUND]) return rgb(*o);
        return background;
    }
    return foreground;
}

Color TermPalette::indexed(std::uint8_t i) const {
    const auto level = [](unsigned v) { return static_cast<std::uint8_t>(v == 0 ? 0 : 55 + v * 40); };
    if (i < 16) return ansi[i];
    if (i < 232) {
        const unsigned n = i - 16U;
        return Color::rgb8(level(n / 36), level(n / 6 % 6), level(n % 6));
    }
    const auto g = static_cast<std::uint8_t>(8 + (i - 232) * 10);
    return Color::rgb8(g, g, g);
}

} // namespace loa::ui::terminal
