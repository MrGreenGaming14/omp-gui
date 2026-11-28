#pragma once

// XLSX workbooks: cell queries for the agent, and a grid renderer with one
// page per worksheet. Port of `loa-surfaces/src/docs/xlsx.rs`; the workbook
// reader replaces calamine 0.38 with the same cell semantics (types, dates,
// durations, errors, shared formulas).

#include <cstdint>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/surfaces/docs/format.hpp"

namespace loa::surfaces::docs {

namespace xlsx {
// Rows and columns in an Excel worksheet.
inline constexpr std::uint32_t MAX_ROWS = 1'048'576;
inline constexpr std::uint32_t MAX_COLUMNS = 16'384;
// Rows and columns drawn on a rendered page, counted from A1.
inline constexpr std::uint32_t RENDER_ROWS = 50;
inline constexpr std::uint32_t RENDER_COLUMNS = 20;
// Grid geometry at scale 1, in points.
inline constexpr float COLUMN_WIDTH = 64.0F;
inline constexpr float ROW_HEIGHT = 20.0F;
inline constexpr float ROW_HEADER_WIDTH = 40.0F;
inline constexpr float CELL_PADDING = 3.0F;

// The letters of 0-based column `col` (`A`, `Z`, `AA`, ...).
[[nodiscard]] std::string column_name(std::uint32_t col);
// The A1 address of a 0-based (row, column) position.
[[nodiscard]] std::string address(std::uint32_t row, std::uint32_t col);
// A number the way the General format shows it.
[[nodiscard]] std::string general(double v);
} // namespace xlsx

// Most cells one `XlsxDoc::cells` call returns.
inline constexpr std::uint64_t MAX_BLOCK_CELLS = 10'000;

// A worksheet as listed to the agent.
struct SheetInfo {
    std::string name;
    // Size of the used range (the box around every non-empty cell).
    std::uint32_t rows = 0;
    std::uint32_t columns = 0;
    // Top left cell of the used range, like `A1`; none for an empty sheet.
    std::optional<std::string> start;

    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const SheetInfo&, const SheetInfo&) = default;
};

// A cell value with its type. JSON: `{"type": "float", "value": 1.5}`;
// `empty` has no `value`.
struct Cell {
    struct Empty {
        friend bool operator==(const Empty&, const Empty&) = default;
    };
    struct String {
        std::string value;
        friend bool operator==(const String&, const String&) = default;
    };
    struct Float {
        double value = 0;
        friend bool operator==(const Float&, const Float&) = default;
    };
    struct Int {
        std::int64_t value = 0;
        friend bool operator==(const Int&, const Int&) = default;
    };
    struct Bool {
        bool value = false;
        friend bool operator==(const Bool&, const Bool&) = default;
    };
    // An error value such as `#DIV/0!`.
    struct Error {
        std::string value;
        friend bool operator==(const Error&, const Error&) = default;
    };
    // A date and/or time: the Excel serial number when stored, and ISO 8601.
    struct DateTime {
        std::optional<double> serial;
        std::string iso;
        friend bool operator==(const DateTime&, const DateTime&) = default;
    };
    // A time span: serial days when stored, and an ISO 8601 duration such as `PT1H30M0S`.
    struct Duration {
        std::optional<double> serial;
        std::string iso;
        friend bool operator==(const Duration&, const Duration&) = default;
    };

    std::variant<Empty, String, Float, Int, Bool, Error, DateTime, Duration> v;

    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const Cell&, const Cell&) = default;
};

struct CellOut {
    // Absolute A1 address, like `B3`.
    std::string address;
    Cell value;
    // Formula without the leading `=`; `value` is its cached result.
    std::optional<std::string> formula;

    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const CellOut&, const CellOut&) = default;
};

// A rectangle of cells, row by row.
struct CellBlock {
    std::string sheet;
    // The rectangle returned, in A1 notation.
    std::string range;
    std::vector<std::vector<CellOut>> rows;

    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const CellBlock&, const CellBlock&) = default;
};

// A parsed XLSX workbook: every worksheet in workbook order with its values and formulas.
class XlsxDoc final : public Paged {
  public:
    [[nodiscard]] static Result<std::unique_ptr<XlsxDoc>> parse(std::span<const std::uint8_t> bytes);
    ~XlsxDoc() override;

    [[nodiscard]] std::vector<SheetInfo> sheets() const;
    // The cells of `range` (`B2:D5` or `C3`) on `sheet` (the first sheet when
    // none), with absolute addresses. The bottom right corner is pulled in to
    // the sheet's used range (never above or left of the top left corner), and
    // at most `MAX_BLOCK_CELLS` cells are returned.
    [[nodiscard]] Result<CellBlock> cells(std::optional<std::string_view> sheet, std::string_view range) const;

    [[nodiscard]] std::size_t page_count() const override;
    [[nodiscard]] Result<PageSize> page_size(std::size_t index) const override;
    [[nodiscard]] Result<Rgba> render(std::size_t index, float scale, const Fonts& fonts, const RenderOpts& opts) const override;
    [[nodiscard]] std::optional<Result<json::Value>> query(const DataQuery& query) const override;

    struct Impl;

  private:
    explicit XlsxDoc(std::unique_ptr<Impl> impl);
    std::unique_ptr<Impl> impl_;
};

} // namespace loa::surfaces::docs
