// The only TU of loa::surfaces that includes Qt.
#include "loa/surfaces/docs/pdf.hpp"

#include <QBuffer>
#include <QByteArray>
#include <QImage>
#include <QPdfDocument>
#include <QPdfSelection>
#include <QPolygonF>

#include <algorithm>
#include <cmath>
#include <format>
#include <limits>
#include <mutex>

namespace loa::surfaces::docs {

struct PdfDoc::Impl {
    QByteArray bytes;
    QBuffer buffer;
    QPdfDocument document;
    // QPdfDocument is not reentrant; one call at a time per document.
    mutable std::mutex mutex;
};

namespace {

// Fraction of the text height two segments' centers may differ by and still share a line.
constexpr double LINE_TOLERANCE = 0.4;

struct Point {
    double x = 0, y = 0;
};

// A run of text pdfium lays out on one line, with its box in reading space
// (`x` along the line, `y` down the page).
struct Segment {
    QString chars; // UTF-16 while collecting, so surrogate pairs stay whole
    std::string text;
    QRectF rect;
    Point center;
    double x0 = 0;
    // Extent across the baseline.
    double size = 0;
};

bool is_space(char c) { return c == ' ' || c == '\t' || c == '\r' || c == '\n'; }

std::string trimmed(std::string s) {
    std::size_t a = 0, b = s.size();
    while (a < b && is_space(s[a])) ++a;
    while (b > a && is_space(s[b - 1])) --b;
    return s.substr(a, b - a);
}

// The page's text in reading order: lines top to bottom joined with `\n`,
// words separated by a space. Invisible text (OCR layers) is included.
// pdfium splits the page into line segments (with its own word spacing); the
// segments are then ordered as hayro's glyph layout in `pdf.rs` orders glyphs.
std::string extract_text(QPdfDocument& doc, int page) {
    const QString all = doc.getAllText(page).text();
    std::vector<Segment> segments(1);
    std::array<std::size_t, 4> votes{};
    std::optional<QPointF> last;
    for (int i = 0; i < all.size(); ++i) {
        const QChar c = all.at(i);
        if (c == QLatin1Char('\r') || c == QLatin1Char('\n')) {
            if (!segments.back().chars.isEmpty()) segments.emplace_back();
            continue;
        }
        segments.back().chars += c;
        if (c.isSpace()) continue;
        const QRectF rect = doc.getSelectionAtIndex(page, i, 1).boundingRectangle();
        if (rect.isEmpty()) continue;
        auto& seg = segments.back();
        seg.rect = seg.rect.isEmpty() ? rect : seg.rect.united(rect);
        // Each step to the next character votes for the reading direction, in
        // quarter turns clockwise from left to right.
        if (last) {
            const QPointF d = rect.center() - *last;
            if (std::abs(d.x()) >= std::abs(d.y())) ++votes[d.x() >= 0 ? 0 : 2];
            else ++votes[d.y() >= 0 ? 1 : 3];
        }
        last = rect.center();
    }
    std::erase_if(segments, [](Segment& s) {
        s.text = trimmed(s.chars.toStdString());
        return s.text.empty();
    });
    if (segments.empty()) return {};

    // Read along the direction most characters run in, so rotated text still comes out line by line.
    std::size_t quadrant = 0;
    for (std::size_t q = 1; q < 4; ++q)
        if (votes[q] > votes[quadrant]) quadrant = q;
    constexpr std::array<std::pair<double, double>, 4> turns{{{0.0, 1.0}, {1.0, 0.0}, {0.0, -1.0}, {-1.0, 0.0}}};
    const auto [sin, cos] = turns[quadrant];
    const auto rotate = [sin, cos](QPointF p) { return Point{p.x() * cos + p.y() * sin, p.y() * cos - p.x() * sin}; };
    for (auto& s : segments) {
        s.center = rotate(s.rect.center());
        const Point a = rotate(s.rect.topLeft()), b = rotate(s.rect.bottomRight());
        s.x0 = std::min(a.x, b.x);
        s.size = std::abs(a.y - b.y);
    }
    std::ranges::stable_sort(segments, [](const Segment& a, const Segment& b) {
        return a.center.y != b.center.y ? a.center.y < b.center.y : a.x0 < b.x0;
    });

    std::string out;
    std::size_t start = 0;
    while (start < segments.size()) {
        const double base = segments[start].center.y;
        double line_size = segments[start].size;
        std::size_t end = start + 1;
        while (end < segments.size() &&
               segments[end].center.y - base <= LINE_TOLERANCE * std::max(line_size, segments[end].size)) {
            line_size = std::max(line_size, segments[end].size);
            ++end;
        }
        std::stable_sort(segments.begin() + static_cast<std::ptrdiff_t>(start), segments.begin() + static_cast<std::ptrdiff_t>(end),
                         [](const Segment& a, const Segment& b) { return a.x0 < b.x0; });
        if (!out.empty()) out.push_back('\n');
        for (std::size_t i = start; i < end; ++i) {
            if (i > start) out.push_back(' ');
            out += segments[i].text;
        }
        start = end;
    }
    return out;
}

json::Value strings(const std::vector<std::string>& texts) {
    json::Array array;
    array.reserve(texts.size());
    for (const auto& t : texts) array.emplace_back(t);
    return array;
}

} // namespace

PdfDoc::PdfDoc(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}
PdfDoc::~PdfDoc() = default;

Result<std::unique_ptr<PdfDoc>> PdfDoc::parse(std::span<const std::uint8_t> bytes) {
    auto impl = std::make_unique<Impl>();
    impl->bytes = QByteArray(reinterpret_cast<const char*>(bytes.data()), static_cast<qsizetype>(bytes.size()));
    impl->buffer.setBuffer(&impl->bytes);
    impl->buffer.open(QIODevice::ReadOnly);
    impl->document.load(&impl->buffer);
    switch (impl->document.error()) {
        case QPdfDocument::Error::None: break;
        case QPdfDocument::Error::IncorrectPassword:
        case QPdfDocument::Error::UnsupportedSecurityScheme:
            return std::unexpected(DocsError::invalid(Format::Pdf, "cannot decrypt the document (password required)"));
        default: return std::unexpected(DocsError::invalid(Format::Pdf, "not a readable PDF document"));
    }
    if (impl->document.status() != QPdfDocument::Status::Ready)
        return std::unexpected(DocsError::invalid(Format::Pdf, "not a readable PDF document"));
    return std::unique_ptr<PdfDoc>(new PdfDoc(std::move(impl)));
}

std::vector<std::string> PdfDoc::text() const {
    std::lock_guard lock(impl_->mutex);
    std::vector<std::string> out;
    for (int i = 0; i < impl_->document.pageCount(); ++i) out.push_back(extract_text(impl_->document, i));
    return out;
}

std::size_t PdfDoc::page_count() const {
    std::lock_guard lock(impl_->mutex);
    return static_cast<std::size_t>(std::max(impl_->document.pageCount(), 0));
}

Result<PageSize> PdfDoc::page_size(std::size_t index) const {
    std::lock_guard lock(impl_->mutex);
    const auto count = static_cast<std::size_t>(std::max(impl_->document.pageCount(), 0));
    if (index >= count) return std::unexpected(DocsError::page_range(index, count));
    // The size with the page's /Rotate applied, as hayro's `render_dimensions`.
    const QSizeF size = impl_->document.pagePointSize(static_cast<int>(index));
    return PageSize{static_cast<float>(size.width()), static_cast<float>(size.height())};
}

std::optional<Result<json::Value>> PdfDoc::query(const DataQuery& query) const {
    const auto* text_query = std::get_if<query::Text>(&query.v);
    if (text_query == nullptr) return std::nullopt;
    if (!text_query->page) return Result<json::Value>(strings(text()));
    std::lock_guard lock(impl_->mutex);
    const auto count = static_cast<std::size_t>(std::max(impl_->document.pageCount(), 0));
    if (*text_query->page >= count) return Result<json::Value>(std::unexpected(DocsError::page_range(*text_query->page, count)));
    return Result<json::Value>(json::Value(extract_text(impl_->document, static_cast<int>(*text_query->page))));
}

Result<Rgba> PdfDoc::render(std::size_t index, float scale, const Fonts&, const RenderOpts&) const {
    auto size = page_size(index);
    if (!size) return std::unexpected(std::move(size.error()));
    if (!(std::isfinite(scale) && scale > 0.0F)) return std::unexpected(DocsError::request(std::format("invalid render scale {}", scale)));
    const auto [width, height] = size->pixels(scale);
    constexpr std::uint32_t max = std::numeric_limits<std::uint16_t>::max();
    if (width > max || height > max)
        return std::unexpected(DocsError::request(
            std::format("page {} at scale {} is {} x {} pixels, the limit is {} x {}", index, scale, width, height, max, max)));

    QImage image;
    {
        std::lock_guard lock(impl_->mutex);
        image = impl_->document.render(static_cast<int>(index), QSize(static_cast<int>(width), static_cast<int>(height)));
    }
    if (image.isNull() || static_cast<std::uint32_t>(image.width()) != width || static_cast<std::uint32_t>(image.height()) != height)
        return std::unexpected(DocsError::invalid(Format::Pdf, std::format("page {} could not be rendered", index)));
    image = image.convertToFormat(QImage::Format_RGBA8888);
    Rgba out = Rgba::make(width, height);
    for (std::uint32_t y = 0; y < height; ++y) {
        const uchar* row = image.constScanLine(static_cast<int>(y));
        std::uint8_t* dst = out.data.data() + static_cast<std::size_t>(y) * width * 4;
        for (std::uint32_t x = 0; x < width * 4; x += 4) {
            // Composited onto a white page, as hayro's `TargetInit::Clear(WHITE)`.
            const unsigned a = row[x + 3];
            for (int c = 0; c < 3; ++c) dst[x + c] = static_cast<std::uint8_t>((row[x + c] * a + 255U * (255U - a) + 127U) / 255U);
            dst[x + 3] = 255;
        }
    }
    return out;
}

} // namespace loa::surfaces::docs
