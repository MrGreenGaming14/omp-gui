#pragma once

// DOCX documents read with pugixml (the parts and semantics `docx-rs` reads):
// an owned outline for the agent (paragraphs with heading level, list
// numbering and run formatting, and tables) and a flow layout renderer, not
// Word's: blocks run top to bottom inside the margins, headings bigger and
// bold, list items indented under a marker, tables as equal width cells. Port
// of `loa-surfaces/src/docs/docx.rs`.

#include <array>
#include <cstddef>
#include <cstdint>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <utility>
#include <variant>
#include <vector>

#include "loa/surfaces/docs/format.hpp"

namespace loa::surfaces::docs {

namespace docx {

// Page used when the document has no section properties: US Letter.
inline constexpr PageSize LETTER{612.0F, 792.0F};
// Margin on every side when the document has no section properties.
inline constexpr float DEFAULT_MARGIN = 72.0F;
inline constexpr float TWIPS_PER_POINT = 20.0F;
// Indent per list level in points.
inline constexpr float LIST_INDENT = 18.0F;
// Padding between a table cell's border and its text, in points.
inline constexpr float CELL_PADDING = 4.0F;
// Advance of a tab, in ems. Matches the box metric the layout is made with.
inline constexpr float TAB_EM = 0.6F;
// Font sizes of the title and headings 1 to 3; deeper headings use 12 pt.
inline constexpr std::array<float, 4> HEADING_SIZES{26.0F, 20.0F, 16.0F, 14.0F};
// List levels OOXML defines (`ilvl` 0 to 8).
inline constexpr std::size_t LIST_LEVELS = 9;
// Bound on `basedOn` chains, which a malformed file can make cyclic.
inline constexpr std::size_t MAX_STYLE_DEPTH = 16;

struct Run {
    std::string text;
    bool bold = false;
    bool italic = false;
    bool underline = false;

    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const Run&, const Run&) = default;
};

// List membership of a paragraph (`w:numPr`).
struct ListItem {
    // Numbering instance (`w:numId`).
    std::size_t num_id = 0;
    // Nesting level (`w:ilvl`), from 0.
    std::uint8_t level = 0;
    // The rendered marker, such as `1.`, `a)` or `•`.
    std::string marker;

    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const ListItem&, const ListItem&) = default;
};

struct Paragraph {
    // Paragraph style id (`w:pStyle`), as used in the document.
    std::optional<std::string> style;
    // 0 for a title, 1 to 9 for headings, from the style's name or id.
    std::optional<std::uint8_t> heading_level;
    std::optional<ListItem> list;
    std::vector<Run> runs;
    // The runs' text concatenated; tabs are `\t`, line and page breaks `\n`.
    std::string text;
    // The paragraph starts on a new page (`w:pageBreakBefore`).
    bool page_break_before = false;
    // The paragraph holds a page break; the layout starts a new page after it.
    bool page_break_after = false;

    // The fields without the `type` tag.
    [[nodiscard]] json::Object fields() const;
    friend bool operator==(const Paragraph&, const Paragraph&) = default;
};

// A table; each cell is its paragraphs' text joined by `\n`.
struct Table {
    std::vector<std::vector<std::string>> rows;

    friend bool operator==(const Table&, const Table&) = default;
};

// `{"type": "paragraph", ...}` or `{"type": "table", "rows": [...]}`.
struct Block {
    std::variant<Paragraph, Table> v;

    [[nodiscard]] const Paragraph* paragraph() const noexcept { return std::get_if<Paragraph>(&v); }
    [[nodiscard]] const Table* table() const noexcept { return std::get_if<Table>(&v); }
    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const Block&, const Block&) = default;
};

// The document's content in reading order.
struct Outline {
    std::vector<Block> blocks;

    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const Outline&, const Outline&) = default;
};

struct Margins {
    float top = 0;
    float right = 0;
    float bottom = 0;
    float left = 0;
    friend bool operator==(const Margins&, const Margins&) = default;
};

// Drawing operations at scale 1, in points.
namespace op {
// Bytes `start..end` of the paragraph at `block`, or of its `(row, col)` cell, top edge at `y`.
struct Text {
    float x = 0, y = 0, size = 0;
    std::size_t block = 0;
    std::optional<std::pair<std::size_t, std::size_t>> cell;
    std::size_t start = 0, end = 0;
    friend bool operator==(const Text&, const Text&) = default;
};
// The list marker of the paragraph at `block`.
struct Marker {
    float x = 0, y = 0, size = 0;
    std::size_t block = 0;
    friend bool operator==(const Marker&, const Marker&) = default;
};
struct Border {
    float x = 0, y = 0, w = 0, h = 0;
    friend bool operator==(const Border&, const Border&) = default;
};
} // namespace op

using Op = std::variant<op::Text, op::Marker, op::Border>;
using Pages = std::vector<std::vector<Op>>;

// Lays the outline out on pages; there is always at least one page.
[[nodiscard]] Pages layout(const Outline& outline, PageSize page, Margins margins, const Fonts& fonts, float body_size);

// Heading level of a style name (`heading 1`, `Title`) or id (`Heading1`).
[[nodiscard]] std::optional<std::uint8_t> heading_level_of(std::string_view label);
// Formats a list counter in an OOXML number format (`w:numFmt`).
[[nodiscard]] std::string format_number(std::uint32_t n, std::string_view format);

} // namespace docx

// A parsed DOCX file. Pagination uses box metrics from `Fonts::none`,
// independent of drawing fonts. Default pages are cached; other body sizes are
// laid out using the supplied rendering options. A table row taller than a
// page is clipped. All pages take the size of the document's final section.
class DocxDoc final : public Paged {
  public:
    [[nodiscard]] static Result<std::unique_ptr<DocxDoc>> parse(std::span<const std::uint8_t> bytes);

    // Plain text: paragraphs joined by `\n`, table cells by `\t`, rows by `\n`.
    [[nodiscard]] std::string text() const;
    [[nodiscard]] const docx::Outline& outline() const noexcept { return outline_; }
    [[nodiscard]] PageSize page() const noexcept { return page_; }
    [[nodiscard]] docx::Margins margins() const noexcept { return margins_; }
    // Pages laid out at the default body size.
    [[nodiscard]] const docx::Pages& pages() const noexcept { return pages_; }

    [[nodiscard]] std::size_t page_count() const override;
    [[nodiscard]] Result<PageSize> page_size(std::size_t index) const override;
    [[nodiscard]] std::size_t page_count_with_opts(const RenderOpts& opts) const override;
    [[nodiscard]] Result<PageSize> page_size_with_opts(std::size_t index, const RenderOpts& opts) const override;
    [[nodiscard]] Result<Rgba> render(std::size_t index, float scale, const Fonts& fonts, const RenderOpts& opts) const override;
    [[nodiscard]] std::optional<Result<json::Value>> query(const DataQuery& query) const override;

  private:
    DocxDoc(docx::Outline outline, PageSize page, docx::Margins margins, docx::Pages pages)
        : outline_(std::move(outline)), page_(page), margins_(margins), pages_(std::move(pages)) {}
    void draw_op(Canvas& canvas, const Fonts& fonts, float scale, const RenderOpts& opts, const docx::Op& op) const;

    docx::Outline outline_;
    PageSize page_;
    docx::Margins margins_;
    docx::Pages pages_;
};

} // namespace loa::surfaces::docs
