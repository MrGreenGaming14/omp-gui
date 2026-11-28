#include "loa/ui/surface/geometry.hpp"

#include <algorithm>
#include <cmath>
#include <limits>

namespace loa::ui::surface {

std::optional<Placement> fit(std::uint32_t width, std::uint32_t height, Size panel) {
    const Size surface{static_cast<float>(width), static_cast<float>(height)};
    if (surface.width <= 0 || surface.height <= 0 || panel.width <= 0 || panel.height <= 0) return std::nullopt;
    const float scale = std::min(panel.width / surface.width, panel.height / surface.height);
    const Size drawn{surface.width * scale, surface.height * scale};
    const Rectangle rect{(panel.width - drawn.width) / 2.0F, (panel.height - drawn.height) / 2.0F, drawn.width, drawn.height};
    return Placement(rect, scale, surface);
}

Point Placement::unclamped(Point at) const { return {(at.x - rect.x) / scale, (at.y - rect.y) / scale}; }

std::optional<Point> Placement::to_surface(Point at) const {
    const Point p = unclamped(at);
    if (p.x >= 0 && p.y >= 0 && p.x < surface_.width && p.y < surface_.height) return p;
    return std::nullopt;
}

Point Placement::to_surface_clamped(Point at) const {
    const Point p = unclamped(at);
    const float inf = std::numeric_limits<float>::infinity();
    return {std::clamp(p.x, 0.0F, std::nextafter(surface_.width, -inf)), std::clamp(p.y, 0.0F, std::nextafter(surface_.height, -inf))};
}

Point Placement::to_panel(Point at) const { return {rect.x + at.x * scale, rect.y + at.y * scale}; }

} // namespace loa::ui::surface
