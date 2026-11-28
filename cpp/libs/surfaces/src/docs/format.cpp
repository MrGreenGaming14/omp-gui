#include "loa/surfaces/docs/format.hpp"

#include <algorithm>
#include <cmath>
#include <cstring>
#include <format>

#include "loa/surfaces/docs/images.hpp"
#include "loa/surfaces/docs/pdf.hpp"

namespace loa::surfaces::docs {

namespace {

namespace cs = loa::contracts::surface;

bool ieq(std::string_view a, std::string_view b) {
    return a.size() == b.size() && std::ranges::equal(a, b, [](char x, char y) {
               const auto lower = [](char c) { return (c >= 'A' && c <= 'Z') ? static_cast<char>(c - 'A' + 'a') : c; };
               return lower(x) == lower(y);
           });
}

bool starts_with(std::span<const std::uint8_t> bytes, std::string_view prefix, std::size_t at = 0) {
    return bytes.size() >= at + prefix.size() && std::memcmp(bytes.data() + at, prefix.data(), prefix.size()) == 0;
}

Result<std::unique_ptr<Paged>> open_pdf(Format, std::span<const std::uint8_t> bytes) {
    auto doc = PdfDoc::parse(bytes);
    if (!doc) return std::unexpected(std::move(doc.error()));
    return std::unique_ptr<Paged>(std::move(*doc));
}

constexpr std::string_view PDF_EXTS[] = {"pdf"};
constexpr std::string_view XLSX_EXTS[] = {"xlsx", "xlsm"};
constexpr std::string_view DOCX_EXTS[] = {"docx"};
constexpr std::string_view PPTX_EXTS[] = {"pptx"};
constexpr std::string_view PNG_EXTS[] = {"png"};
constexpr std::string_view JPEG_EXTS[] = {"jpg", "jpeg"};
constexpr std::string_view GIF_EXTS[] = {"gif"};
constexpr std::string_view WEBP_EXTS[] = {"webp"};
constexpr std::string_view BMP_EXTS[] = {"bmp"};

constexpr Sniff magic(std::string_view m) { return {Sniff::Kind::Magic, m}; }
constexpr Sniff zip_part(std::string_view p) { return {Sniff::Kind::ZipPart, p}; }
constexpr Sniff image(ImageFormat f) { return {Sniff::Kind::Image, {}, f}; }

const DocFormat FORMATS[] = {
    {Format::Pdf, "PDF", "application/pdf", PDF_EXTS, magic("%PDF-"), open_pdf},
    {Format::Xlsx, "XLSX", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", XLSX_EXTS,
     zip_part("xl/workbook.xml"), nullptr},
    {Format::Docx, "DOCX", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", DOCX_EXTS,
     zip_part("word/document.xml"), nullptr},
    {Format::Pptx, "PPTX", "application/vnd.openxmlformats-officedocument.presentationml.presentation", PPTX_EXTS,
     zip_part("ppt/presentation.xml"), nullptr},
    {Format::Png, "PNG", "image/png", PNG_EXTS, image(ImageFormat::Png), images::open},
    {Format::Jpeg, "JPEG", "image/jpeg", JPEG_EXTS, image(ImageFormat::Jpeg), images::open},
    {Format::Gif, "GIF", "image/gif", GIF_EXTS, image(ImageFormat::Gif), images::open},
    {Format::Webp, "WebP", "image/webp", WEBP_EXTS, image(ImageFormat::WebP), images::open},
    {Format::Bmp, "BMP", "image/bmp", BMP_EXTS, image(ImageFormat::Bmp), images::open},
};

constexpr std::string_view SERDE_NAMES[] = {"pdf", "xlsx", "docx", "pptx", "png", "jpeg", "gif", "webp", "bmp"};

// The `image` crate's `guess_format`, for the formats the docs server opens.
std::optional<ImageFormat> guess_image(std::span<const std::uint8_t> bytes) {
    if (starts_with(bytes, "\x89PNG\r\n\x1a\n")) return ImageFormat::Png;
    if (starts_with(bytes, "\xff\xd8\xff")) return ImageFormat::Jpeg;
    if (starts_with(bytes, "GIF89a") || starts_with(bytes, "GIF87a")) return ImageFormat::Gif;
    if (starts_with(bytes, "RIFF") && starts_with(bytes, "WEBP", 8)) return ImageFormat::WebP;
    if (starts_with(bytes, "BM")) return ImageFormat::Bmp;
    return std::nullopt;
}

} // namespace

// --- DocsError ---

DocsError DocsError::io(std::filesystem::path path, std::string source) {
    DocsError e;
    e.kind_ = Kind::Io;
    e.text_ = path.string();
    e.source_ = std::move(source);
    return e;
}

DocsError DocsError::invalid(Format format, std::string message) {
    DocsError e;
    e.kind_ = Kind::Invalid;
    e.format_ = format;
    e.text_ = std::move(message);
    return e;
}

DocsError DocsError::page_range(std::size_t index, std::size_t count) {
    DocsError e;
    e.kind_ = Kind::PageRange;
    e.index_ = index;
    e.count_ = count;
    return e;
}

DocsError DocsError::unsupported(std::string what) {
    DocsError e;
    e.kind_ = Kind::Unsupported;
    e.text_ = std::move(what);
    return e;
}

DocsError DocsError::request(std::string message) {
    DocsError e;
    e.kind_ = Kind::Request;
    e.text_ = std::move(message);
    return e;
}

DocsError DocsError::pixels(PixelError error) {
    DocsError e;
    e.kind_ = Kind::Pixels;
    e.pixel_ = std::move(error);
    return e;
}

DocsError DocsError::surface(cs::SurfaceError error) {
    DocsError e;
    e.kind_ = Kind::Surface;
    e.surface_ = std::move(error);
    return e;
}

std::string DocsError::message() const {
    switch (kind_) {
        case Kind::Io: return std::format("reading {}: {}", text_, source_);
        case Kind::Invalid: return std::format("invalid {} file: {}", to_string(*format_), text_);
        case Kind::PageRange: return std::format("page {} is out of range, the document has {}", index_, count_);
        case Kind::Unsupported: return std::format("unsupported file type `{}`", text_);
        case Kind::Request: return text_;
        case Kind::Pixels: return pixel_->message();
        case Kind::Surface: return surface_->message();
    }
    return text_;
}

cs::SurfaceError DocsError::to_surface_error() const {
    if (kind_ == Kind::Surface) return *surface_;
    return cs::SurfaceError::backend(message());
}

// --- Format ---

std::span<const DocFormat> formats() { return FORMATS; }

const DocFormat& spec(Format format) { return FORMATS[static_cast<std::size_t>(format)]; }

std::string_view mime(Format format) { return spec(format).mime; }

std::string_view to_string(Format format) { return spec(format).name; }

std::string_view serde_name(Format format) { return SERDE_NAMES[static_cast<std::size_t>(format)]; }

std::optional<Format> from_mime(std::string_view m) {
    for (const auto& f : FORMATS)
        if (ieq(f.mime, m)) return f.format;
    return std::nullopt;
}

std::optional<Format> from_extension(std::string_view ext) {
    for (const auto& f : FORMATS)
        for (auto e : f.exts)
            if (ieq(e, ext)) return f.format;
    return std::nullopt;
}

std::optional<Format> detect(const std::filesystem::path& path, std::span<const std::uint8_t> bytes) {
    // Rust `Path::extension`: none for dotfiles like `.pdf` and for names ending in a dot.
    const std::string ext = path.extension().string();
    if (ext.size() > 1)
        if (auto f = from_extension(std::string_view(ext).substr(1))) return f;
    return sniff(bytes);
}

std::optional<Format> sniff(std::span<const std::uint8_t> bytes) {
    const auto img = guess_image(bytes);
    const bool zip = starts_with(bytes, std::string_view("PK\x03\x04", 4));
    for (const auto& f : FORMATS) {
        const Sniff& s = f.sniff;
        bool hit = false;
        switch (s.kind) {
            case Sniff::Kind::Magic: hit = starts_with(bytes, s.bytes); break;
            case Sniff::Kind::ZipPart: {
                const std::string_view hay(reinterpret_cast<const char*>(bytes.data()), bytes.size());
                hit = zip && hay.find(s.bytes) != std::string_view::npos;
                break;
            }
            case Sniff::Kind::Image: hit = img == s.image; break;
        }
        if (hit) return f.format;
    }
    return std::nullopt;
}

bool is_image(Format format) { return spec(format).sniff.kind == Sniff::Kind::Image; }

// --- PageSize ---

std::pair<std::uint32_t, std::uint32_t> PageSize::pixels(float scale) const {
    // Rust `as u32` saturates and maps NaN to 0.
    const auto px = [scale](float v) {
        const float r = std::round(v * scale);
        std::uint32_t n = 0;
        if (r >= 4294967295.0F) n = UINT32_MAX;
        else if (r > 0) n = static_cast<std::uint32_t>(r);
        return std::max<std::uint32_t>(n, 1);
    };
    return {px(width), px(height)};
}

json::Value PageSize::to_json() const {
    return json::Object{{"width", static_cast<double>(width)}, {"height", static_cast<double>(height)}};
}

// --- DataQuery ---

std::string_view DataQuery::what() const {
    constexpr std::string_view names[] = {"info", "page text", "sheets", "cells", "outline", "slides"};
    return names[v.index()];
}

json::Value DataQuery::to_json() const {
    return std::visit(
        [](const auto& q) -> json::Value {
            using T = std::decay_t<decltype(q)>;
            const auto opt = [](const auto& o) -> json::Value {
                if (!o) return nullptr;
                if constexpr (std::is_same_v<std::decay_t<decltype(*o)>, std::size_t>)
                    return static_cast<std::uint64_t>(*o);
                else
                    return *o;
            };
            if constexpr (std::is_same_v<T, query::Info>) return json::Object{{"kind", "info"}};
            else if constexpr (std::is_same_v<T, query::Text>) return json::Object{{"kind", "text"}, {"page", opt(q.page)}};
            else if constexpr (std::is_same_v<T, query::Sheets>) return json::Object{{"kind", "sheets"}};
            else if constexpr (std::is_same_v<T, query::Cells>)
                return json::Object{{"kind", "cells"}, {"sheet", opt(q.sheet)}, {"range", q.range}};
            else if constexpr (std::is_same_v<T, query::Outline>) return json::Object{{"kind", "outline"}};
            else return json::Object{{"kind", "slides"}};
        },
        v);
}

std::expected<DataQuery, loa::Error> DataQuery::from_json(const json::Value& value) {
    const auto fail = [](std::string m) { return std::unexpected(loa::Error(loa::ErrorCode::parse, std::move(m))); };
    if (!value.is_object()) return fail("invalid type: expected internally tagged enum DataQuery");
    const auto* kind = value.find("kind");
    if (kind == nullptr) return fail("missing field `kind`");
    const auto* k = kind->as_string();
    if (k == nullptr) return fail("invalid type: expected variant identifier");
    if (*k == "info") return DataQuery{query::Info{}};
    if (*k == "sheets") return DataQuery{query::Sheets{}};
    if (*k == "outline") return DataQuery{query::Outline{}};
    if (*k == "slides") return DataQuery{query::Slides{}};
    if (*k == "text") {
        query::Text t;
        if (const auto* page = value.find("page"); page != nullptr && !page->is_null()) {
            auto n = page->as_u64();
            if (!n) return fail("invalid type: expected usize for `page`");
            t.page = static_cast<std::size_t>(*n);
        }
        return DataQuery{t};
    }
    if (*k == "cells") {
        query::Cells c;
        if (const auto* sheet = value.find("sheet"); sheet != nullptr && !sheet->is_null()) {
            const auto* s = sheet->as_string();
            if (s == nullptr) return fail("invalid type: expected a string for `sheet`");
            c.sheet = *s;
        }
        const auto* range = value.find("range");
        if (range == nullptr) return fail("missing field `range`");
        const auto* r = range->as_string();
        if (r == nullptr) return fail("invalid type: expected a string for `range`");
        c.range = *r;
        return DataQuery{c};
    }
    return fail(std::format("unknown variant `{}`, expected one of `info`, `text`, `sheets`, `cells`, `outline`, `slides`", *k));
}

Result<std::unique_ptr<Paged>> open_pages(Format format, std::span<const std::uint8_t> bytes) {
    const auto open = spec(format).open;
    if (open == nullptr) return std::unexpected(DocsError::unsupported(std::format("{} documents are not supported yet", to_string(format))));
    return open(format, bytes);
}

} // namespace loa::surfaces::docs
