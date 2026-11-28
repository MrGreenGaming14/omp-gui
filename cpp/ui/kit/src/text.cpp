#include "loa/ui/kit/text.hpp"

#include <algorithm>
#include <climits>
#include <cmath>
#include <format>
#include <limits>
#include <tuple>

#include <QChar>
#include <QFont>
#include <QFontDatabase>
#include <QFontMetricsF>
#include <QString>

namespace loa::ui::kit::text {
namespace {

// Byte offsets of every code point start in `s`.
std::vector<std::size_t> boundaries(std::string_view s) {
    std::vector<std::size_t> out;
    out.reserve(s.size());
    for (std::size_t i = 0; i < s.size(); ++i)
        if ((static_cast<unsigned char>(s[i]) & 0xC0) != 0x80) out.push_back(i);
    return out;
}

std::size_t char_count(std::string_view s) { return boundaries(s).size(); }

// Decodes `s` into code points (invalid bytes map to U+FFFD).
template <typename F> void for_each_char(std::string_view s, F&& f) {
    std::size_t i = 0;
    while (i < s.size()) {
        const auto b = static_cast<unsigned char>(s[i]);
        std::size_t len = b < 0x80 ? 1 : (b >> 5) == 0x6 ? 2 : (b >> 4) == 0xE ? 3 : (b >> 3) == 0x1E ? 4 : 1;
        if (i + len > s.size()) len = 1;
        char32_t c = len == 1 ? (b < 0x80 ? b : 0xFFFD) : b & (0x7F >> len);
        for (std::size_t k = 1; k < len; ++k) c = (c << 6) | (static_cast<unsigned char>(s[i + k]) & 0x3F);
        f(i, len, c);
        i += len;
    }
}

// Lower case when it is a single char (Rust `to_lowercase` yielding one char), else `c`.
char32_t fold(char32_t c) {
    if (c == 0x130) return c; // `İ` lowercases to two chars
    return QChar::toLower(static_cast<char32_t>(c));
}

bool is_alnum(char32_t c) { return QChar::isLetterOrNumber(c); }

// Whether `c` starts a word: first character, after a non-alphanumeric, or a camelCase hump.
bool is_boundary(std::optional<char32_t> prev, char32_t c) {
    return !prev || !is_alnum(*prev) || (QChar::isLower(*prev) && QChar::isUpper(c));
}

constexpr std::int32_t BASE = 16;
constexpr std::int32_t BOUNDARY_BONUS = 8;
constexpr std::int32_t CONSECUTIVE_BONUS = 12;
constexpr std::int32_t MAX_LEADING_PENALTY = 15;
constexpr std::int32_t NONE = std::numeric_limits<std::int32_t>::min() / 2;

std::int32_t to_int(std::size_t n) {
    return n > static_cast<std::size_t>(std::numeric_limits<std::int32_t>::max()) ? std::numeric_limits<std::int32_t>::max() / 4
                                                                                  : static_cast<std::int32_t>(n);
}

std::int32_t leading(std::size_t j) { return std::min(to_int(j), MAX_LEADING_PENALTY); }

} // namespace

void Matcher::set_query(std::string_view query) {
    query_.clear();
    for_each_char(query, [this](std::size_t, std::size_t, char32_t c) {
        if (!QChar::isSpace(c)) query_.push_back(fold(c));
    });
}

std::int32_t Matcher::base(std::size_t j) const { return BASE + (boundary_[j] ? BOUNDARY_BONUS : 0); }

std::int32_t Matcher::score_of(std::span<const std::size_t> indices) const {
    std::int32_t total = 0;
    for (std::size_t i = 0; i < indices.size(); ++i) {
        const auto j = indices[i];
        total += base(j);
        if (i == 0) total -= leading(j);
        else if (j == indices[i - 1] + 1) total += CONSECUTIVE_BONUS;
        else total -= to_int(j - indices[i - 1] - 1);
    }
    return total;
}

std::optional<Match> Matcher::matches(std::string_view title) {
    if (query_.empty()) return Match{Class::Prefix, 0, {}};
    title_.clear();
    boundary_.clear();
    std::optional<char32_t> prev;
    for_each_char(title, [&](std::size_t, std::size_t, char32_t c) {
        boundary_.push_back(is_boundary(prev, c));
        title_.push_back(fold(c));
        prev = c;
    });
    const auto m = query_.size();
    if (m > title_.size()) return std::nullopt;
    for (std::size_t j = 0; j + m <= title_.size(); ++j) {
        if (!boundary_[j] || !std::equal(query_.begin(), query_.end(), title_.begin() + static_cast<std::ptrdiff_t>(j))) continue;
        std::vector<std::size_t> indices(m);
        for (std::size_t k = 0; k < m; ++k) indices[k] = j + k;
        const auto score = score_of(indices);
        return Match{j == 0 ? Class::Prefix : Class::WordPrefix, score, std::move(indices)};
    }
    if (auto a = align(true)) return Match{Class::WordBoundary, a->first, std::move(a->second)};
    if (auto a = align(false)) return Match{Class::Fuzzy, a->first, std::move(a->second)};
    return std::nullopt;
}

std::optional<std::pair<std::int32_t, std::vector<std::size_t>>> Matcher::align(bool word_runs) {
    const auto m = query_.size(), n = title_.size();
    score_.assign(m * n, NONE);
    parent_.assign(m * n, 0);
    for (std::size_t j = 0; j < n; ++j)
        if (title_[j] == query_[0] && (!word_runs || boundary_[j])) score_[j] = base(j) - leading(j);
    for (std::size_t i = 1; i < m; ++i) {
        const auto prev_row = (i - 1) * n, row = i * n;
        // Best score[i-1][k] + k over k < j - 1 for gap transitions.
        std::optional<std::pair<std::int32_t, std::size_t>> best_gap;
        for (std::size_t j = 1; j < n; ++j) {
            if (j >= 2 && score_[prev_row + j - 2] > NONE) {
                const auto v = score_[prev_row + j - 2] + to_int(j - 2);
                if (!best_gap || v > best_gap->first) best_gap = std::pair{v, j - 2};
            }
            if (title_[j] != query_[i]) continue;
            const auto b = base(j);
            std::int32_t best = NONE;
            std::size_t from = 0;
            if (score_[prev_row + j - 1] > NONE) {
                best = score_[prev_row + j - 1] + b + CONSECUTIVE_BONUS;
                from = j - 1;
            }
            if (best_gap && (!word_runs || boundary_[j])) {
                const auto [v, k] = *best_gap;
                if (v - to_int(j - 1) + b > best) {
                    best = v - to_int(j - 1) + b;
                    from = k;
                }
            }
            score_[row + j] = best;
            parent_[row + j] = from;
        }
    }
    const auto last = (m - 1) * n;
    std::optional<std::size_t> end;
    for (std::size_t j = 0; j < n; ++j)
        if (score_[last + j] > NONE && (!end || score_[last + j] > score_[last + *end])) end = j;
    if (!end) return std::nullopt;
    const auto score = score_[last + *end];
    std::vector<std::size_t> indices(m);
    auto j = *end;
    for (std::size_t i = m; i-- > 0;) {
        indices[i] = j;
        j = parent_[i * n + j];
    }
    return std::pair{score, std::move(indices)};
}

std::vector<Run> highlight_runs(std::string_view title, std::span<const std::size_t> indices) {
    std::vector<Run> runs;
    std::size_t next = 0, i = 0;
    for_each_char(title, [&](std::size_t byte, std::size_t len, char32_t) {
        const bool matched = next < indices.size() && indices[next] == i;
        if (matched) ++next;
        if (!runs.empty() && runs.back().matched == matched)
            runs.back().end = byte + len;
        else
            runs.push_back({byte, byte + len, matched});
        ++i;
    });
    return runs;
}

std::vector<std::size_t> filter(std::string_view query, std::span<const std::string_view> candidates, std::size_t limit) {
    Matcher m;
    m.set_query(query);
    std::vector<std::tuple<Class, std::int32_t, std::size_t, std::size_t>> hits;
    for (std::size_t i = 0; i < candidates.size(); ++i)
        if (auto h = m.matches(candidates[i])) hits.emplace_back(h->cls, -h->score, m.is_empty() ? 0 : candidates[i].size(), i);
    std::ranges::sort(hits);
    std::vector<std::size_t> out;
    for (std::size_t k = 0; k < hits.size() && k < limit; ++k) out.push_back(std::get<3>(hits[k]));
    return out;
}

std::string format_duration(std::int64_t ms) {
    if (ms < 1'000) return std::format("{:.1f}s", static_cast<double>(ms) / 1000.0);
    if (ms < 60'000) return std::format("{}s", ms / 1000);
    if (ms < 3'600'000) return std::format("{}m {}s", ms / 60'000, ms % 60'000 / 1000);
    return std::format("{}h {}m", ms / 3'600'000, ms % 3'600'000 / 60'000);
}

std::string format_bytes(std::uint64_t bytes) {
    auto v = static_cast<double>(bytes);
    for (std::string_view unit : {"B", "KB", "MB", "GB"}) {
        if (v < 1024.0 || unit == "GB") return unit == "B" ? std::format("{} B", bytes) : std::format("{:.1f} {}", v, unit);
        v /= 1024.0;
    }
    return {};
}

std::string truncate_trailing(std::string_view s, std::size_t max) {
    const auto b = boundaries(s);
    const auto nth = max == 0 ? 0 : max - 1;
    if (nth < b.size() && b.size() > max) return std::string(s.substr(0, b[nth])) + std::string(ELLIPSIS);
    return std::string(s);
}

std::string truncate_leading(std::string_view s, std::size_t max) {
    const auto b = boundaries(s);
    const auto n = b.size();
    if (n <= max) return std::string(s);
    const auto k = n + 1 - std::max<std::size_t>(max, 1);
    const auto skip = k < n ? b[k] : s.size();
    return std::string(ELLIPSIS) + std::string(s.substr(skip));
}

namespace {

std::string ellipsize_end(std::string_view text, float max_width, const Measure& measure) {
    if (text.empty() || max_width <= 0.0F || std::isnan(max_width)) return {};
    if (max_width == std::numeric_limits<float>::infinity() || measure(text) <= max_width) return std::string(text);
    if (measure(ELLIPSIS) > max_width) return {};
    const auto b = boundaries(text);
    std::size_t lo = 0, hi = b.size();
    std::string buf;
    buf.reserve(text.size() + ELLIPSIS.size());
    auto build = [&](std::size_t k) {
        buf.assign(text.substr(0, b[k]));
        buf += ELLIPSIS;
    };
    while (lo + 1 < hi) {
        const auto mid = lo + (hi - lo) / 2;
        build(mid);
        if (measure(buf) <= max_width) lo = mid;
        else hi = mid;
    }
    build(lo);
    return buf;
}

std::string ellipsize_path(std::string_view text, float max_width, const Measure& measure) {
    auto trimmed = text;
    while (!trimmed.empty() && (trimmed.back() == '/' || trimmed.back() == '\\')) trimmed.remove_suffix(1);
    if (trimmed.empty()) return ellipsize_end(text, max_width, measure);
    if (trimmed.size() < text.size() && measure(trimmed) <= max_width) return std::string(trimmed);
    const auto sep_idx = trimmed.find_last_of("/\\");
    if (sep_idx == std::string_view::npos || sep_idx == 0) return ellipsize_end(text, max_width, measure);
    const auto parent = trimmed.substr(0, sep_idx);
    const auto tail = trimmed.substr(sep_idx + 1);
    const auto suffix = std::string(ELLIPSIS) + trimmed[sep_idx] + std::string(tail);
    if (measure(suffix) > max_width) return ellipsize_end(tail, max_width, measure);
    const auto b = boundaries(parent);
    std::size_t lo = 0, hi = b.size();
    std::string buf;
    auto build = [&](std::size_t k) {
        buf.assign(parent.substr(0, b[k]));
        buf += suffix;
    };
    while (lo + 1 < hi) {
        const auto mid = lo + (hi - lo) / 2;
        build(mid);
        if (measure(buf) <= max_width) lo = mid;
        else hi = mid;
    }
    build(lo);
    return buf;
}

} // namespace

std::string ellipsize_with(std::string_view text, float max_width, EllipsisMode mode, const Measure& measure) {
    if (text.empty() || max_width <= 0.0F || std::isnan(max_width)) return {};
    if (max_width == std::numeric_limits<float>::infinity() || measure(text) <= max_width) return std::string(text);
    return mode == EllipsisMode::End ? ellipsize_end(text, max_width, measure) : ellipsize_path(text, max_width, measure);
}

float measure_text_width(std::string_view content, float size, theme::Font font) {
    if (content.empty() || size <= 0.0F || !std::isfinite(size)) return 0.0F;
    QFont f = font == theme::Font::Monospace ? QFontDatabase::systemFont(QFontDatabase::FixedFont) : QFont();
    f.setPixelSize(std::max(1, static_cast<int>(std::lround(size))));
    const QFontMetricsF metrics(f);
    return static_cast<float>(metrics.horizontalAdvance(QString::fromUtf8(content.data(), static_cast<qsizetype>(content.size()))));
}

std::string ellipsize_text(std::string_view content, float max_width, float size, theme::Font font, EllipsisMode mode) {
    return ellipsize_with(content, max_width, mode, [size, font](std::string_view s) { return measure_text_width(s, size, font); });
}

} // namespace loa::ui::kit::text
