#pragma once

// PPTX presentations: slide text and shape geometry for the agent, and a
// simple slide renderer. Port of `loa-surfaces/src/docs/pptx.rs`.

#include <cstdint>
#include <map>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <vector>

#include "loa/surfaces/docs/format.hpp"

namespace loa::surfaces::docs {

enum class ShapeKind : std::uint8_t { Text, Picture, Table, Other };

// Serde name (`text`, `picture`, `table`, `other`).
[[nodiscard]] std::string_view to_string(ShapeKind kind);

// How a paragraph is drawn, from its first run's properties. Not serialized.
struct ParaStyle {
    // Font size in points.
    std::optional<float> size;
    bool bold = false;
    friend bool operator==(const ParaStyle&, const ParaStyle&) = default;
};

// A shape on a slide. Geometry is in points from the slide's top left corner,
// after group transforms; placeholders without their own geometry take it from
// the slide layout or master.
struct Shape {
    std::string name;
    ShapeKind kind = ShapeKind::Other;
    // The `p:ph` type (`title`, `ctrTitle`, `body`, `subTitle`, `obj`, ...).
    std::optional<std::string> placeholder;
    float x = 0, y = 0, width = 0, height = 0;
    // One entry per paragraph, line breaks as `\n`. Tables give one entry per
    // row, cells separated by ` | `.
    std::vector<std::string> paragraphs;
    // Part path of the picture (`ppt/media/image1.png`).
    std::optional<std::string> image;
    // Drawing style of each entry of `paragraphs`.
    std::vector<ParaStyle> styles;

    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const Shape&, const Shape&) = default;
};

struct Slide {
    // Position in presentation order, from 0 (the page index).
    std::size_t index = 0;
    // Text of the first title or centered title placeholder.
    std::optional<std::string> title;
    // Non empty paragraphs (table rows for tables) of the other text shapes, in document order.
    std::vector<std::string> body;
    // Text of the notes slide's body placeholder.
    std::optional<std::string> notes;
    std::vector<Shape> shapes;

    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const Slide&, const Slide&) = default;
};

// A parsed presentation: slides in presentation order and the bytes of the pictures they show.
class PptxDoc final : public Paged {
  public:
    [[nodiscard]] static Result<std::unique_ptr<PptxDoc>> parse(std::span<const std::uint8_t> bytes);

    // Plain text: per slide a `Slide N` header, the title, the body paragraphs and the notes.
    [[nodiscard]] std::string text() const;
    [[nodiscard]] const PageSize& size() const noexcept { return size_; }
    [[nodiscard]] const std::vector<Slide>& slides() const noexcept { return slides_; }

    [[nodiscard]] std::size_t page_count() const override { return slides_.size(); }
    [[nodiscard]] Result<PageSize> page_size(std::size_t index) const override;
    [[nodiscard]] Result<Rgba> render(std::size_t index, float scale, const Fonts& fonts, const RenderOpts& opts) const override;
    [[nodiscard]] std::optional<Result<json::Value>> query(const DataQuery& query) const override;

  private:
    PptxDoc() = default;
    PageSize size_;
    std::vector<Slide> slides_;
    // Picture bytes by part path (`ppt/media/image1.png`).
    std::map<std::string, std::vector<std::uint8_t>, std::less<>> media_;
};

} // namespace loa::surfaces::docs
