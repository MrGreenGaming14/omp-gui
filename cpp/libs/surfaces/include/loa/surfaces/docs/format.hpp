#pragma once

// Document surfaces: PDF, XLSX, DOCX, PPTX and images. Port of
// `loa-surfaces/src/docs/mod.rs`.
//
// `FORMATS` registers each format (MIME type, extensions, magic, parser); a
// parsed file is a `Paged` document the viewer renders locally and the
// `DocsServer` (`surface://docs/<id>`) serves and queries.

#include <array>
#include <cstddef>
#include <cstdint>
#include <expected>
#include <filesystem>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <utility>
#include <variant>

#include "loa/contracts/surface.hpp"
#include "loa/runtime/json.hpp"
#include "loa/surfaces/docs/raster.hpp"
#include "loa/surfaces/pixels.hpp"

namespace loa::surfaces::docs {

namespace json = loa::runtime::json;

// The file formats the `docs` server opens.
enum class Format : std::uint8_t { Pdf, Xlsx, Docx, Pptx, Png, Jpeg, Gif, Webp, Bmp };

class DocsError {
  public:
    enum class Kind : std::uint8_t { Io, Invalid, PageRange, Unsupported, Request, Pixels, Surface };

    [[nodiscard]] static DocsError io(std::filesystem::path path, std::string source);
    [[nodiscard]] static DocsError invalid(Format format, std::string message);
    [[nodiscard]] static DocsError page_range(std::size_t index, std::size_t count);
    [[nodiscard]] static DocsError unsupported(std::string what);
    // A query that doesn't fit the document (unknown sheet, bad range).
    [[nodiscard]] static DocsError request(std::string message);
    [[nodiscard]] static DocsError pixels(PixelError error);
    [[nodiscard]] static DocsError surface(loa::contracts::surface::SurfaceError error);

    [[nodiscard]] Kind kind() const noexcept { return kind_; }
    // Set for `Invalid`.
    [[nodiscard]] std::optional<Format> format() const noexcept { return format_; }
    // Set for `PageRange`.
    [[nodiscard]] std::size_t index() const noexcept { return index_; }
    [[nodiscard]] std::size_t count() const noexcept { return count_; }
    // The Rust `Display` text.
    [[nodiscard]] std::string message() const;
    // `From<DocsError> for SurfaceError`: the inner surface error, else `Backend(message)`.
    [[nodiscard]] loa::contracts::surface::SurfaceError to_surface_error() const;

  private:
    DocsError() = default;
    Kind kind_ = Kind::Request;
    std::optional<Format> format_;
    std::size_t index_ = 0, count_ = 0;
    std::string text_; // path, message or what, by kind
    std::string source_;
    std::optional<PixelError> pixel_;
    std::optional<loa::contracts::surface::SurfaceError> surface_;
};

template <typename T>
using Result = std::expected<T, DocsError>;

// Image formats sniffed by their magic, as the `image` crate's `guess_format`.
enum class ImageFormat : std::uint8_t { Png, Jpeg, Gif, WebP, Bmp };

// How a file of a format is recognised by its content.
struct Sniff {
    enum class Kind : std::uint8_t { Magic, ZipPart, Image };
    Kind kind;
    // Magic prefix (Magic) or zip part name (ZipPart).
    std::string_view bytes;
    ImageFormat image = ImageFormat::Png;
};

class Paged;
using Opener = Result<std::unique_ptr<Paged>> (*)(Format, std::span<const std::uint8_t>);

struct DocFormat {
    Format format;
    std::string_view name;
    std::string_view mime;
    std::span<const std::string_view> exts;
    Sniff sniff;
    // Null for formats not built yet (the OOXML parsers live in S5b).
    Opener open;
};

// Every format, in sniffing order.
[[nodiscard]] std::span<const DocFormat> formats();

[[nodiscard]] const DocFormat& spec(Format format);
[[nodiscard]] std::string_view mime(Format format);
// The display name, e.g. `PDF`.
[[nodiscard]] std::string_view to_string(Format format);
// The serde name, e.g. `pdf`.
[[nodiscard]] std::string_view serde_name(Format format);
[[nodiscard]] std::optional<Format> from_mime(std::string_view mime);
// From a file extension, case insensitive.
[[nodiscard]] std::optional<Format> from_extension(std::string_view ext);
// From the extension of `path`, falling back to the file's magic bytes.
[[nodiscard]] std::optional<Format> detect(const std::filesystem::path& path, std::span<const std::uint8_t> bytes);
// From magic bytes; Office files are zip archives told apart by their main part.
[[nodiscard]] std::optional<Format> sniff(std::span<const std::uint8_t> bytes);
[[nodiscard]] bool is_image(Format format);

// Size of a page at scale 1, in points (1/72 inch), which are also surface
// pixels at scale 1.
struct PageSize {
    float width = 0;
    float height = 0;

    // Pixel size of the page rendered at `scale`, at least 1 x 1.
    [[nodiscard]] std::pair<std::uint32_t, std::uint32_t> pixels(float scale) const;
    [[nodiscard]] json::Value to_json() const;
    friend bool operator==(const PageSize&, const PageSize&) = default;
};

// Structured data the agent tools ask a document surface for. JSON shape:
// serde `#[serde(tag = "kind", rename_all = "snake_case")]`.
namespace query {
// Format, MIME type, page count and page sizes. Every format.
struct Info {
    friend bool operator==(const Info&, const Info&) = default;
};
// Page text (PDF, one entry per page or just `page`), document text (DOCX) or slide text (PPTX).
struct Text {
    std::optional<std::size_t> page;
    friend bool operator==(const Text&, const Text&) = default;
};
// XLSX: sheet names and used sizes.
struct Sheets {
    friend bool operator==(const Sheets&, const Sheets&) = default;
};
// XLSX: cells of `range` (A1 notation) in `sheet` (default: the first).
struct Cells {
    std::optional<std::string> sheet;
    std::string range;
    friend bool operator==(const Cells&, const Cells&) = default;
};
// DOCX: paragraphs with style, heading level, list level and runs, and tables.
struct Outline {
    friend bool operator==(const Outline&, const Outline&) = default;
};
// PPTX: slides in presentation order with their shapes, text and notes.
struct Slides {
    friend bool operator==(const Slides&, const Slides&) = default;
};
} // namespace query

struct DataQuery {
    std::variant<query::Info, query::Text, query::Sheets, query::Cells, query::Outline, query::Slides> v;

    // What the query asks for, for the error when a format lacks it.
    [[nodiscard]] std::string_view what() const;
    [[nodiscard]] json::Value to_json() const;
    // Accepts missing `page` and `sheet`, as `#[serde(default)]`.
    [[nodiscard]] static std::expected<DataQuery, loa::Error> from_json(const json::Value& value);

    friend bool operator==(const DataQuery&, const DataQuery&) = default;
};

// A parsed document: pages (a PDF page, a sheet, a DOCX page, a slide or the
// image) and structured data.
class Paged {
  public:
    virtual ~Paged() = default;

    [[nodiscard]] virtual std::size_t page_count() const = 0;
    // Size of page `index` (from 0) at scale 1.
    [[nodiscard]] virtual Result<PageSize> page_size(std::size_t index) const = 0;
    // Page count for the supplied rendering options.
    [[nodiscard]] virtual std::size_t page_count_with_opts(const RenderOpts&) const { return page_count(); }
    // Page size for the supplied rendering options.
    [[nodiscard]] virtual Result<PageSize> page_size_with_opts(std::size_t index, const RenderOpts&) const {
        return page_size(index);
    }
    // Renders page `index` at `scale` (1.0 = one pixel per point) with `opts`.
    [[nodiscard]] virtual Result<Rgba> render(std::size_t index, float scale, const Fonts& fonts, const RenderOpts& opts) const = 0;
    [[nodiscard]] Result<Rgba> render_page(std::size_t index, float scale, const Fonts& fonts) const {
        return render(index, scale, fonts, RenderOpts{});
    }
    // Answers `query`, or `std::nullopt` when the format has no such data.
    [[nodiscard]] virtual std::optional<Result<json::Value>> query(const DataQuery&) const { return std::nullopt; }
};

// Parses a document for local rendering on the viewing side.
[[nodiscard]] Result<std::unique_ptr<Paged>> open_pages(Format format, std::span<const std::uint8_t> bytes);

} // namespace loa::surfaces::docs
