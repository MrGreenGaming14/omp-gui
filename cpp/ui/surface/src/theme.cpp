#include "loa/ui/surface/theme.hpp"

#include <array>
#include <cmath>
#include <utility>

#include "loa/ui/kit/theme.hpp"

namespace loa::ui::surface {

namespace {

using Icon = loa::view::theme::Icon;

constexpr std::array<std::pair<std::string_view, SurfaceKindStyle>, 4> KINDS{{
    {BROWSER, {Icon::Globe, "Browser"}},
    {loa::view::surface::DOCUMENT_SERVER, {Icon::FileText, "Document"}},
    {VM, {Icon::Monitor, "VM or desktop (VNC)"}},
    {MCP, {Icon::Wrench, "From MCP servers"}},
}};

std::uint8_t channel(float v) { return static_cast<std::uint8_t>(std::lround(std::clamp(v, 0.0F, 1.0F) * 255.0F)); }

// iced `Color::into_rgba8`.
std::array<std::uint8_t, 4> rgba8(kit::theme::Color c) { return {channel(c.r), channel(c.g), channel(c.b), channel(c.a)}; }

} // namespace

SurfaceKindStyle SurfaceKindStyle::of(std::string_view provider, loa::view::surface::SurfaceMode mode) {
    for (const auto& [name, style] : KINDS)
        if (name == provider) return style;
    if (mode == loa::view::surface::SurfaceMode::Document) return KINDS[1].second;
    return {Icon::Square, ""};
}

loa::view::surface::port::DocumentPalette document_palette() {
    namespace t = kit::theme;
    auto active = t::tokens();
    auto set = active->is_dark() ? t::builtin_tokens(t::Variant::Light) : active;
    const auto c = [&](t::ColorRole r) { return rgba8(set->color(r)); };
    return {c(t::ColorRole::Card), c(t::ColorRole::Text), c(t::ColorRole::Text3), c(t::ColorRole::Line), c(t::ColorRole::Hover),
            c(t::ColorRole::Text2)};
}

} // namespace loa::ui::surface
