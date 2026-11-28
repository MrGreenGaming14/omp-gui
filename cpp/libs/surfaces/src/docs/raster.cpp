#include "loa/surfaces/docs/raster.hpp"

#include <algorithm>
#include <cctype>
#include <cmath>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <system_error>

// The one TU with the stb_truetype implementation, private to this file.
#define STB_TRUETYPE_IMPLEMENTATION
#define STBTT_STATIC
#include <stb/stb_truetype.h>

namespace loa::surfaces::docs {

class Font {
  public:
    static std::shared_ptr<const Font> load(const std::filesystem::path& path) {
        std::ifstream in(path, std::ios::binary);
        if (!in) return nullptr;
        std::vector<unsigned char> data((std::istreambuf_iterator<char>(in)), std::istreambuf_iterator<char>());
        if (data.empty()) return nullptr;
        auto font = std::shared_ptr<Font>(new Font(std::move(data)));
        const int offset = stbtt_GetFontOffsetForIndex(font->data_.data(), 0);
        if (offset < 0 || stbtt_InitFont(&font->info_, font->data_.data(), offset) == 0) return nullptr;
        return font;
    }

    // ab_glyph `PxScale::from(size)`: `size` is the ascent to descent height.
    [[nodiscard]] float scale(float size) const { return stbtt_ScaleForPixelHeight(&info_, size); }
    [[nodiscard]] float ascent(float size) const {
        int ascent = 0, descent = 0, gap = 0;
        stbtt_GetFontVMetrics(&info_, &ascent, &descent, &gap);
        return static_cast<float>(ascent) * scale(size);
    }
    [[nodiscard]] int glyph(char32_t c) const { return stbtt_FindGlyphIndex(&info_, static_cast<int>(c)); }
    [[nodiscard]] float advance(int glyph, float size) const {
        int advance = 0, bearing = 0;
        stbtt_GetGlyphHMetrics(&info_, glyph, &advance, &bearing);
        return static_cast<float>(advance) * scale(size);
    }
    [[nodiscard]] float kern(int a, int b, float size) const {
        return static_cast<float>(stbtt_GetGlyphKernAdvance(&info_, a, b)) * scale(size);
    }
    [[nodiscard]] const stbtt_fontinfo& info() const noexcept { return info_; }

  private:
    explicit Font(std::vector<unsigned char> data) : data_(std::move(data)) {}
    std::vector<unsigned char> data_;
    stbtt_fontinfo info_{};
};

namespace {

// Font files known by name, most preferred first.
constexpr std::string_view REGULAR_NAMES[] = {
    "DejaVuSans.ttf", "LiberationSans-Regular.ttf", "NotoSans-Regular.ttf", "FreeSans.ttf", "Ubuntu-R.ttf",
    "Arial.ttf",      "arial.ttf",                  "SFNS.ttf",             "segoeui.ttf",
};
constexpr std::string_view BOLD_NAMES[] = {
    "DejaVuSans-Bold.ttf", "LiberationSans-Bold.ttf", "NotoSans-Bold.ttf", "FreeSansBold.ttf",
    "Ubuntu-B.ttf",        "Arial Bold.ttf",          "arialbd.ttf",       "segoeuib.ttf",
};
// System wide font directories on Linux, macOS and Windows.
constexpr std::string_view SYSTEM_FONT_DIRS[] = {"/usr/share/fonts", "/usr/local/share/fonts", "/System/Library/Fonts",
                                                 "/Library/Fonts", "C:\\Windows\\Fonts"};
// How deep font directories are searched.
constexpr std::size_t MAX_DEPTH = 4;
// Advance of a box drawn for a character without a font, in ems.
constexpr float BOX_ADVANCE = 0.6F;

bool ieq(std::string_view a, std::string_view b) {
    return a.size() == b.size() && std::ranges::equal(a, b, [](char x, char y) {
               return std::tolower(static_cast<unsigned char>(x)) == std::tolower(static_cast<unsigned char>(y));
           });
}

void collect_font_files(const std::filesystem::path& dir, std::size_t depth, std::vector<std::filesystem::path>& out) {
    std::error_code ec;
    std::filesystem::directory_iterator it(dir, ec);
    if (ec) return;
    for (; it != std::filesystem::directory_iterator(); it.increment(ec)) {
        if (ec) return;
        const auto& entry = *it;
        // `DirEntry::file_type` does not follow symlinks.
        const auto status = entry.symlink_status(ec);
        if (ec) continue;
        const bool is_dir = std::filesystem::is_directory(status);
        const std::string ext = entry.path().extension().string();
        const std::string_view e = ext.empty() ? std::string_view{} : std::string_view(ext).substr(1);
        if (is_dir && depth > 0) {
            collect_font_files(entry.path(), depth - 1, out);
        } else if (!is_dir && (ieq(e, "ttf") || ieq(e, "otf"))) {
            out.push_back(entry.path());
        }
    }
}

// Decodes the next UTF-8 scalar at `i`, advancing it; invalid bytes become U+FFFD.
char32_t next_char(std::string_view s, std::size_t& i) {
    const auto b = static_cast<unsigned char>(s[i]);
    int len = 1;
    char32_t c = b;
    if (b >= 0xf0) len = 4, c = b & 0x07;
    else if (b >= 0xe0) len = 3, c = b & 0x0f;
    else if (b >= 0xc0) len = 2, c = b & 0x1f;
    else if (b >= 0x80) { ++i; return 0xfffd; }
    if (i + static_cast<std::size_t>(len) > s.size()) { i = s.size(); return 0xfffd; }
    for (int k = 1; k < len; ++k) c = (c << 6) | (static_cast<unsigned char>(s[i + k]) & 0x3f);
    i += static_cast<std::size_t>(len);
    return c;
}

std::size_t char_count(std::string_view s) {
    std::size_t n = 0;
    for (std::size_t i = 0; i < s.size(); ++n) next_char(s, i);
    return n;
}

bool is_whitespace(char32_t c) {
    return c == ' ' || (c >= 0x09 && c <= 0x0d) || c == 0x85 || c == 0xa0 || c == 0x1680 || (c >= 0x2000 && c <= 0x200a) ||
           c == 0x2028 || c == 0x2029 || c == 0x202f || c == 0x205f || c == 0x3000;
}

// Lays `text` out with kerning, calling `f(char, glyph, caret, advance)` per character; returns the advance.
template <typename F>
float glyphs(const Font& font, float size, std::string_view text, F&& f) {
    float caret = 0;
    int last = -1;
    for (std::size_t i = 0; i < text.size();) {
        const char32_t c = next_char(text, i);
        const int id = font.glyph(c);
        if (last >= 0) caret += font.kern(last, id, size);
        const float advance = font.advance(id, size);
        f(c, id, caret, advance);
        caret += advance;
        last = id;
    }
    return caret;
}

std::string_view trim_end(std::string_view s) {
    while (!s.empty() && (s.back() == ' ' || s.back() == '\t' || s.back() == '\r' || s.back() == '\n' ||
                          s.back() == '\v' || s.back() == '\f'))
        s.remove_suffix(1);
    return s;
}

} // namespace

// --- Fonts ---

Fonts Fonts::system() {
    std::vector<std::filesystem::path> files;
    for (auto dir : SYSTEM_FONT_DIRS) collect_font_files(std::filesystem::path(dir), MAX_DEPTH, files);
    std::ranges::sort(files);
    const auto find = [&](std::span<const std::string_view> names) -> std::shared_ptr<const Font> {
        for (auto name : names)
            for (const auto& f : files)
                if (f.filename() == name)
                    if (auto font = Font::load(f)) return font;
        return nullptr;
    };
    Fonts fonts;
    fonts.regular_ = find(REGULAR_NAMES);
    if (!fonts.regular_) {
        for (const auto& f : files)
            if ((fonts.regular_ = Font::load(f))) break;
    }
    fonts.bold_ = find(BOLD_NAMES);
    return fonts;
}

const Font* Fonts::pick(bool bold) const noexcept {
    if (bold && bold_) return bold_.get();
    return regular_.get();
}

float Fonts::text_width(std::string_view text, float size, bool bold) const {
    if (const Font* font = pick(bold)) return glyphs(*font, size, text, [](char32_t, int, float, float) {});
    return static_cast<float>(char_count(text)) * size * BOX_ADVANCE;
}

std::vector<std::string_view> Fonts::wrap(std::string_view text, float size, bool bold, float max_width) const {
    std::vector<std::string_view> lines;
    std::size_t from = 0;
    for (;;) {
        const std::size_t nl = text.find('\n', from);
        const std::string_view paragraph = text.substr(from, nl == std::string_view::npos ? std::string_view::npos : nl - from);
        std::size_t start = 0, end = 0, offset = 0;
        // Words with their byte offsets, each with its leading spaces.
        while (offset < paragraph.size()) {
            const std::size_t index = offset;
            const std::string_view rest = paragraph.substr(offset);
            const std::size_t lead = std::min(rest.find_first_not_of(' '), rest.size());
            const std::size_t space = rest.find(' ', lead);
            offset += space == std::string_view::npos ? rest.size() : space;
            const std::string_view word = paragraph.substr(index, offset - index);
            const std::string_view candidate = paragraph.substr(start, index + word.size() - start);
            if (end > start && text_width(trim_end(candidate), size, bold) > max_width) {
                lines.push_back(trim_end(paragraph.substr(start, end - start)));
                start = index + lead;
            }
            end = index + word.size();
        }
        lines.push_back(trim_end(paragraph.substr(start)));
        if (nl == std::string_view::npos) break;
        from = nl + 1;
    }
    return lines;
}

// --- Canvas ---

Canvas::Canvas(std::uint32_t width, std::uint32_t height, Color background)
    : image_(Rgba::filled(width, height, background.rgba)) {}

void Canvas::blend(std::int64_t x, std::int64_t y, Color color, float coverage) {
    if (x < 0 || y < 0 || x >= std::int64_t{image_.width} || y >= std::int64_t{image_.height}) return;
    const float alpha = std::clamp(coverage, 0.0F, 1.0F) * static_cast<float>(color.rgba[3]) / 255.0F;
    if (!(alpha > 0.0F)) return;
    const std::size_t i = (static_cast<std::size_t>(y) * image_.width + static_cast<std::size_t>(x)) * 4;
    std::uint8_t* px = image_.data.data() + i;
    for (int k = 0; k < 3; ++k) {
        const float dst = px[k];
        px[k] = static_cast<std::uint8_t>(std::round(dst + (static_cast<float>(color.rgba[k]) - dst) * alpha));
    }
    const float dst_a = static_cast<float>(px[3]) / 255.0F;
    px[3] = static_cast<std::uint8_t>(std::round((alpha + dst_a * (1.0F - alpha)) * 255.0F));
}

namespace {
// Clamps a float span to pixel indexes inside `0..limit`.
std::pair<std::int64_t, std::int64_t> span(float start, float len, std::uint32_t limit) {
    return {static_cast<std::int64_t>(std::max(std::round(start), 0.0F)),
            std::min(static_cast<std::int64_t>(std::round(start + len)), std::int64_t{limit})};
}
} // namespace

void Canvas::fill_rect(float x, float y, float w, float h, Color color) {
    const auto [x0, x1] = span(x, w, image_.width);
    const auto [y0, y1] = span(y, h, image_.height);
    for (auto py = y0; py < y1; ++py)
        for (auto px = x0; px < x1; ++px) blend(px, py, color, 1.0F);
}

void Canvas::stroke_rect(float x, float y, float w, float h, float thickness, Color color) {
    const float t = std::max(thickness, 1.0F);
    fill_rect(x, y, w, t, color);
    fill_rect(x, y + h - t, w, t, color);
    fill_rect(x, y, t, h, color);
    fill_rect(x + w - t, y, t, h, color);
}

void Canvas::blit(const Rgba& image, float x, float y, float w, float h) {
    if (image.width == 0 || image.height == 0 || w <= 0.0F || h <= 0.0F) return;
    const auto [x0, x1] = span(x, w, image_.width);
    const auto [y0, y1] = span(y, h, image_.height);
    for (auto py = y0; py < y1; ++py) {
        const auto sy = std::min(static_cast<std::uint32_t>(std::max(0.0F, (static_cast<float>(py) + 0.5F - y) / h *
                                                                                static_cast<float>(image.height))),
                                 image.height - 1);
        for (auto px = x0; px < x1; ++px) {
            const auto sx = std::min(static_cast<std::uint32_t>(std::max(0.0F, (static_cast<float>(px) + 0.5F - x) / w *
                                                                                    static_cast<float>(image.width))),
                                     image.width - 1);
            const std::uint8_t* s = image.data.data() + (static_cast<std::size_t>(sy) * image.width + sx) * 4;
            blend(px, py, Color{{s[0], s[1], s[2], 255}}, static_cast<float>(s[3]) / 255.0F);
        }
    }
}

float Canvas::draw_text(const Fonts& fonts, TextStyle style, float x, float y, std::string_view text) {
    const float size = style.size;
    const Color color = style.color;
    const Font* font = fonts.pick(style.bold);
    if (font == nullptr) {
        // One box per character.
        const float advance = size * BOX_ADVANCE, h = size * 0.7F;
        std::size_t n = 0;
        for (std::size_t i = 0; i < text.size(); ++n) {
            const char32_t c = next_char(text, i);
            if (!is_whitespace(c))
                stroke_rect(x + static_cast<float>(n) * advance + 1.0F, y + size * 0.25F, std::max(advance - 2.0F, 1.0F), h, 1.0F,
                            color);
        }
        return static_cast<float>(n) * advance;
    }
    const float scale = font->scale(size);
    const float baseline = y + font->ascent(size);
    return glyphs(*font, size, text, [&](char32_t c, int id, float caret, float advance) {
        caret += x;
        if (id == 0 && !is_whitespace(c)) {
            const float h = size * 0.7F;
            stroke_rect(caret + 1.0F, baseline - h, std::max(advance - 2.0F, 1.0F), h, 1.0F, color);
            return;
        }
        const float fx = std::floor(caret), fy = std::floor(baseline);
        const float shift_x = caret - fx, shift_y = baseline - fy;
        int x0 = 0, y0 = 0, x1 = 0, y1 = 0;
        stbtt_GetGlyphBitmapBoxSubpixel(&font->info(), id, scale, scale, shift_x, shift_y, &x0, &y0, &x1, &y1);
        const int w = x1 - x0, h = y1 - y0;
        if (w <= 0 || h <= 0) return;
        std::vector<unsigned char> bitmap(static_cast<std::size_t>(w) * static_cast<std::size_t>(h));
        stbtt_MakeGlyphBitmapSubpixel(&font->info(), bitmap.data(), w, h, w, scale, scale, shift_x, shift_y, id);
        for (int gy = 0; gy < h; ++gy)
            for (int gx = 0; gx < w; ++gx) {
                const unsigned char cov = bitmap[static_cast<std::size_t>(gy) * static_cast<std::size_t>(w) + static_cast<std::size_t>(gx)];
                if (cov != 0)
                    blend(static_cast<std::int64_t>(fx) + x0 + gx, static_cast<std::int64_t>(fy) + y0 + gy, color,
                          static_cast<float>(cov) / 255.0F);
            }
    });
}

} // namespace loa::surfaces::docs
