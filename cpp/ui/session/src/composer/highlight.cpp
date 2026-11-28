#include "loa/ui/session/composer/highlight.hpp"

#include <algorithm>
#include <array>
#include <charconv>
#include <cmath>
#include <optional>

#include "utf8.hpp"

namespace loa::ui::session::composer {

using kit::theme::Color;

// ---- Shimmer ---------------------------------------------------------------------------------

namespace shimmer {

std::uint8_t phase_at(std::int64_t now_ms) {
    const auto ms = (now_ms < 0 ? 0 : now_ms) % CYCLE_MS;
    return static_cast<std::uint8_t>(ms * PHASES / CYCLE_MS);
}

float progress(std::uint8_t phase) { return static_cast<float>(phase) / static_cast<float>(PHASES); }

std::int64_t now_ms() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(std::chrono::system_clock::now().time_since_epoch()).count();
}

} // namespace shimmer

// ---- Highlighting ----------------------------------------------------------------------------

namespace {

// Colours in one keyword gradient, as in omp.
constexpr std::size_t STOPS = 14;

// A magic keyword: omp acts on it when it stands alone in a prompt.
struct Keyword {
    std::string_view word;
    // Hue at the start and end of the gradient, in degrees.
    float hue_from, hue_to;
    // Lightness at the start and end, in percent; 62 throughout when unset.
    std::optional<std::pair<float, float>> lightness;
};

// omp's magic keywords (`packages/coding-agent/src/modes/magic-keywords.ts`).
const std::array<Keyword, 5> KEYWORDS{{
    {"ultrathink", 0.0F, 330.0F, std::nullopt},
    {"chillax", 180.0F, 235.0F, std::pair{62.0F, 45.0F}},
    {"orchestrate", 150.0F, 280.0F, std::nullopt},
    {"workflowz", 30.0F, 150.0F, std::nullopt},
    {"jevify", 300.0F, 420.0F, std::nullopt},
}};

// Card and token colours of pasted images, by image number.
constexpr std::array<Color, 5> IMAGE_COLORS{
    Color::rgb8(0xff, 0xa6, 0x57), Color::rgb8(0x79, 0xc0, 0xff), Color::rgb8(0xc6, 0x9c, 0xff),
    Color::rgb8(0x7e, 0xe7, 0x87), Color::rgb8(0xff, 0x7b, 0xb0),
};

constexpr Color MENTION = Color::rgb8(0x79, 0xc0, 0xff);

constexpr std::string_view TRAILING = ",.;:!?)]\"'";

bool joins(char32_t c) { return utf8::is_alnum(c) || c == '_'; }

// Where `word` stands alone in `line`, by omp's rule: no letter, digit, `_`, `.`, `/`, `\` or
// `-` against it, no `::` before it, and no `(` or `.name` after it.
std::vector<std::pair<std::size_t, std::size_t>> keyword_ranges(std::string_view line, std::string_view word) {
    std::vector<std::pair<std::size_t, std::size_t>> out;
    for (auto at = line.find(word); at != std::string_view::npos; at = line.find(word, at + word.size())) {
        const auto before = line.substr(0, at);
        const auto after = line.substr(at + word.size());
        if (!before.empty()) {
            const auto prev = utf8::prev(before, before.size());
            if (joins(prev) || prev == '.' || prev == '/' || prev == '\\' || prev == '-') continue;
        }
        if (before.ends_with("::")) continue;
        if (!after.empty()) {
            std::size_t i = 0;
            const auto c = utf8::next(after, i);
            if (joins(c) || c == '/' || c == '\\' || c == '-' || c == '(') continue;
            if (c == '.' && i < after.size()) {
                const auto d = utf8::next(after, i);
                if (joins(d) || d == '-') continue;
            }
        }
        out.emplace_back(at, at + word.size());
    }
    return out;
}

// `@path` and `@"path"` mentions at the start of a word, trailing punctuation left out.
void mentions(std::string_view line, std::vector<Span>& out) {
    std::optional<char32_t> previous;
    for (std::size_t i = 0; i < line.size();) {
        const auto at = i;
        const auto c = utf8::next(line, i);
        const bool starts = c == '@' && (!previous || utf8::is_space(*previous));
        previous = c;
        if (!starts) continue;
        const auto after = line.substr(at + 1);
        if (!after.empty() && after.front() == '"') {
            if (const auto close = after.substr(1).find('"'); close != std::string_view::npos)
                out.push_back({at, at + close + 3, highlight::Mention{}});
            continue;
        }
        const auto space = utf8::find_space(after);
        auto word = after.substr(0, space == std::string_view::npos ? after.size() : space);
        while (!word.empty() && TRAILING.find(word.back()) != std::string_view::npos) word.remove_suffix(1);
        if (!word.empty()) out.push_back({at, at + 1 + word.size(), highlight::Mention{}});
    }
}

// `[Image #n]` tokens and their numbers.
void images(std::string_view line, std::vector<Span>& out) {
    constexpr std::string_view PREFIX = "[Image #";
    for (auto at = line.find(PREFIX); at != std::string_view::npos; at = line.find(PREFIX, at + 1)) {
        const auto rest = line.substr(at + PREFIX.size());
        std::size_t digits = 0;
        while (digits < rest.size() && rest[digits] >= '0' && rest[digits] <= '9') ++digits;
        std::uint32_t n = 0;
        const auto [ptr, ec] = std::from_chars(rest.data(), rest.data() + digits, n);
        if (digits == 0 || ec != std::errc() || ptr != rest.data() + digits) continue;
        if (digits < rest.size() && rest[digits] == ']') out.push_back({at, at + PREFIX.size() + digits + 1, highlight::Image{n}});
    }
}

float rem_euclid(float a, float b) {
    const auto r = std::fmod(a, b);
    return r < 0 ? r + b : r;
}

} // namespace

Color image_color(std::uint32_t n) { return IMAGE_COLORS[(n == 0 ? 0 : n - 1) % IMAGE_COLORS.size()]; }

std::string image_token(std::uint32_t n) { return "[Image #" + std::to_string(n) + "]"; }

Format format(const Highlight& h, const kit::theme::TokenSet& t) {
    return std::visit(
        [&]<class H>(const H& v) -> Format {
            if constexpr (std::is_same_v<H, highlight::Command>) return {t.color(kit::theme::ColorRole::Accent), true};
            else if constexpr (std::is_same_v<H, highlight::Mention>) return {MENTION, false};
            else if constexpr (std::is_same_v<H, highlight::Image>) return {image_color(v.n), true};
            else return {v.color, true};
        },
        h);
}

bool has_keyword(std::string_view line) {
    return std::ranges::any_of(KEYWORDS, [&](const Keyword& k) { return !keyword_ranges(line, k.word).empty(); });
}

std::vector<Span> spans(std::string_view line, bool first, std::uint8_t phase) {
    std::vector<Span> out;
    if (first && line.starts_with('/')) {
        const auto rest = line.substr(1);
        std::size_t name = 0;
        for (std::size_t i = 0; i < rest.size();) {
            const auto at = i;
            const auto c = utf8::next(rest, i);
            if (!(utf8::is_alnum(c) || c == '-' || c == '_' || c == '.' || c == ':')) break;
            name = i;
            (void)at;
        }
        if (name > 0) out.push_back({0, 1 + name, highlight::Command{}});
    }
    mentions(line, out);
    images(line, out);
    for (std::size_t k = 0; k < KEYWORDS.size(); ++k) {
        const auto& keyword = KEYWORDS[k];
        for (const auto [begin, end] : keyword_ranges(line, keyword.word)) {
            if (std::ranges::any_of(out, [&](const Span& s) { return s.begin < end && begin < s.end; })) continue;
            const auto chars = utf8::count(keyword.word);
            std::size_t start = begin, index = 0;
            for (std::size_t i = 0; i < keyword.word.size(); ++index) {
                (void)utf8::next(keyword.word, i);
                out.push_back({start, begin + i, highlight::Keyword{gradient(k, index, chars, phase)}});
                start = begin + i;
            }
        }
    }
    std::ranges::stable_sort(out, {}, &Span::begin);
    return out;
}

Color gradient(std::size_t keyword_index, std::size_t index, std::size_t chars, std::uint8_t phase) {
    const auto& keyword = KEYWORDS[keyword_index];
    const auto shift = shimmer::progress(phase);
    float at = static_cast<float>(index) / static_cast<float>(chars) + shift;
    at -= std::floor(at);
    const auto stop = static_cast<float>(static_cast<std::size_t>(at * static_cast<float>(STOPS)) % STOPS) / static_cast<float>(STOPS);
    const auto hue = rem_euclid(keyword.hue_from + stop * (keyword.hue_to - keyword.hue_from), 360.0F);
    const auto lightness = keyword.lightness ? keyword.lightness->first + stop * (keyword.lightness->second - keyword.lightness->first) : 62.0F;
    return hsl(hue, 0.9F, lightness / 100.0F);
}

Color hsl(float hue, float saturation, float lightness) {
    const auto chroma = (1.0F - std::fabs(2.0F * lightness - 1.0F)) * saturation;
    const auto sector = hue / 60.0F;
    const auto x = chroma * (1.0F - std::fabs(rem_euclid(sector, 2.0F) - 1.0F));
    float r = 0, g = 0, b = 0;
    switch (static_cast<std::uint32_t>(sector)) {
    case 0: r = chroma, g = x; break;
    case 1: r = x, g = chroma; break;
    case 2: g = chroma, b = x; break;
    case 3: g = x, b = chroma; break;
    case 4: r = x, b = chroma; break;
    default: r = chroma, b = x; break;
    }
    const auto m = lightness - chroma / 2.0F;
    return Color{r + m, g + m, b + m, 1.0F};
}

void Highlighter::update(Settings settings) {
    if (settings.phase != phase_) {
        phase_ = settings.phase;
        // Recolour from the top.
        line_ = 0;
    }
}

std::vector<Span> Highlighter::highlight_line(std::string_view line) {
    auto out = spans(line, line_ == 0, phase_);
    ++line_;
    return out;
}

} // namespace loa::ui::session::composer
