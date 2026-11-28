#pragma once

// Mapping between panel and surface coordinates. The surface is scaled to fit the panel with its
// aspect ratio kept and centred (letterboxing); input goes out in surface pixels. Port of
// `crates/loa-ui-surface/src/geometry.rs`.

#include <cstdint>
#include <optional>

namespace loa::ui::surface {

struct Point {
    float x = 0;
    float y = 0;
    friend bool operator==(const Point&, const Point&) = default;
};

struct Size {
    float width = 0;
    float height = 0;
    friend bool operator==(const Size&, const Size&) = default;
};

struct Rectangle {
    float x = 0;
    float y = 0;
    float width = 0;
    float height = 0;
    [[nodiscard]] Point position() const { return {x, y}; }
    friend bool operator==(const Rectangle&, const Rectangle&) = default;
};

// Where a surface of a given size sits inside a panel.
class Placement {
  public:
    // The drawn picture, in panel coordinates.
    Rectangle rect;
    // Panel pixels per surface pixel.
    float scale = 1;

    Placement(Rectangle rect, float scale, Size surface) : rect(rect), scale(scale), surface_(surface) {}

    // The surface point under panel point `at`, `nullopt` on a bar or outside the panel.
    [[nodiscard]] std::optional<Point> to_surface(Point at) const;
    // Like `to_surface` but pinned to the surface edge, for drags that leave the picture with a
    // button held.
    [[nodiscard]] Point to_surface_clamped(Point at) const;
    // The panel point showing surface point `at`.
    [[nodiscard]] Point to_panel(Point at) const;
    // A length in panel pixels expressed in surface pixels.
    [[nodiscard]] float to_surface_len(float len) const { return len / scale; }

    friend bool operator==(const Placement&, const Placement&) = default;

  private:
    [[nodiscard]] Point unclamped(Point at) const;
    Size surface_;
};

// Fits a `width` by `height` surface into `panel`; `nullopt` when either side is empty.
[[nodiscard]] std::optional<Placement> fit(std::uint32_t width, std::uint32_t height, Size panel);

} // namespace loa::ui::surface
