#pragma once

// Software drawing used by the simple page renderers (XLSX grid, DOCX page,
// PPTX slide): an RGBA canvas, rectangles, scaled image blits and text through
// a system font found at runtime. Without a font, text degrades to one
// outlined box per character so the layout stays visible. Port of
// `loa-surfaces/src/docs/raster.rs`.

#include <array>
#include <cstdint>
#include <memory>
#include <string_view>
#include <vector>

#include "loa/surfaces/pixels.hpp"

namespace loa::surfaces::docs {

using Rgba = loa::surfaces::RgbaImage;

// Straight (not premultiplied) RGBA color.
struct Color {
    std::array<std::uint8_t, 4> rgba{};
    friend bool operator==(const Color&, const Color&) = default;
};

// Colours of rendered pages.
struct DocPalette {
    Color page{{255, 255, 255, 255}};
    Color text{{0, 0, 0, 255}};
    // Table borders, header rules and picture placeholders.
    Color rule{{128, 128, 128, 255}};
    // Sheet grid lines and slide table outlines.
    Color grid{{217, 217, 217, 255}};
    Color header{{242, 242, 242, 255}};
    Color header_text{{64, 64, 64, 255}};
    friend bool operator==(const DocPalette&, const DocPalette&) = default;
};

// Default font sizes in points.
struct DocTypography {
    // DOCX body text, including table cells; pagination follows this size.
    float body = 11.0F;
    float cell = 11.0F;
    float slide = 18.0F;
    friend bool operator==(const DocTypography&, const DocTypography&) = default;
};

struct TextStyle {
    // Font size in canvas pixels.
    float size = 0;
    bool bold = false;
    Color color;

    [[nodiscard]] constexpr TextStyle with_bold(bool b) const {
        TextStyle s = *this;
        s.bold = b;
        return s;
    }
    friend bool operator==(const TextStyle&, const TextStyle&) = default;
};

// How pages are drawn; the UI fills it from its theme tokens.
struct RenderOpts {
    DocPalette palette;
    DocTypography typography;

    // Body text of `size` canvas pixels in the palette's text colour.
    [[nodiscard]] TextStyle text(float size) const { return TextStyle{size, false, palette.text}; }
    friend bool operator==(const RenderOpts&, const RenderOpts&) = default;
};

// One loaded font file (stb_truetype); defined in raster.cpp.
class Font;

// The fonts text is drawn with. Cheap to copy.
class Fonts {
  public:
    // No font: text is drawn as boxes.
    [[nodiscard]] static Fonts none() { return {}; }
    // Searches the system font directories for a sans serif font: known
    // families first, else any `.ttf`/`.otf`.
    [[nodiscard]] static Fonts system();

    [[nodiscard]] bool has_regular() const noexcept { return regular_ != nullptr; }
    [[nodiscard]] bool has_bold() const noexcept { return bold_ != nullptr; }

    [[nodiscard]] float line_height(float size) const { return size * 1.25F; }
    // Horizontal advance of `text` (UTF-8) drawn at `size` pixels.
    [[nodiscard]] float text_width(std::string_view text, float size, bool bold) const;
    // Word wraps `text` to `max_width`; newlines always break, a word wider
    // than the line gets its own line. The views point into `text`.
    [[nodiscard]] std::vector<std::string_view> wrap(std::string_view text, float size, bool bold, float max_width) const;

    [[nodiscard]] const Font* pick(bool bold) const noexcept;

  private:
    std::shared_ptr<const Font> regular_;
    std::shared_ptr<const Font> bold_;
};

// An RGBA drawing surface in pixels.
class Canvas {
  public:
    Canvas(std::uint32_t width, std::uint32_t height, Color background);

    [[nodiscard]] std::uint32_t height() const noexcept { return image_.height; }

    void fill_rect(float x, float y, float w, float h, Color color);
    // Outline of a rectangle, `thickness` pixels wide, drawn inside it.
    void stroke_rect(float x, float y, float w, float h, float thickness, Color color);
    // Draws `image` scaled (nearest neighbour) into the given rectangle.
    void blit(const Rgba& image, float x, float y, float w, float h);
    // Draws one line of text with its top edge at `y`. Returns the advance.
    float draw_text(const Fonts& fonts, TextStyle style, float x, float y, std::string_view text);

    [[nodiscard]] const Rgba& rgba() const noexcept { return image_; }
    [[nodiscard]] Rgba into_rgba() && { return std::move(image_); }

  private:
    // Blends `color` at `coverage` (0 to 1) over the pixel at `(x, y)`.
    void blend(std::int64_t x, std::int64_t y, Color color, float coverage);

    Rgba image_;
};

} // namespace loa::surfaces::docs
