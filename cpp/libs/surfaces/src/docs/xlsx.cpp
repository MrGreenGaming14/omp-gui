// XLSX workbooks. Port of `loa-surfaces/src/docs/xlsx.rs`, with an in-house
// workbook reader (ooxml::Package + pugixml) that follows calamine 0.38:
// worksheets from the workbook relationships, shared strings, number formats
// for date and duration detection, cell types, error values and shared
// formulas, with calamine's error texts.
#include "loa/surfaces/docs/xlsx.hpp"

#include <algorithm>
#include <charconv>
#include <cmath>
#include <format>
#include <limits>
#include <map>
#include <new>
#include <stdexcept>
#include <unordered_map>
#include <utility>

#include <pugixml.hpp>

#include "loa/surfaces/docs/ooxml.hpp"

namespace loa::surfaces::docs {

namespace {

using xlsx::MAX_COLUMNS;
using xlsx::MAX_ROWS;

// Most `#` shown instead of a number too wide for its cell, as Excel does.
constexpr std::size_t MAX_HASHES = 64;

// Horizontal alignment as the fraction of the free cell width left of the text.
constexpr float LEFT = 0.0F;
constexpr float CENTER = 0.5F;
constexpr float RIGHT = 1.0F;

// A 0-based (row, column) cell position.
using Pos = std::pair<std::uint32_t, std::uint32_t>;

struct Area {
    Pos start;
    Pos end;
    [[nodiscard]] std::uint64_t len() const {
        return (std::uint64_t{end.first} - start.first + 1) * (std::uint64_t{end.second} - start.second + 1);
    }
};

std::uint64_t key(Pos p) { return (std::uint64_t{p.first} << 32) | p.second; }

DocsError bad(std::string message) { return DocsError::invalid(Format::Xlsx, std::move(message)); }

// ---------------------------------------------------------------------------
// Values as calamine's `Data`.

struct ExcelDateTime {
    double value = 0;
    bool duration = false;
    bool is_1904 = false;
};
struct Text {
    std::string value;
};
struct CellErr {
    std::string value;
};
struct DateTimeIso {
    std::string value;
};
using Data = std::variant<std::monostate, Text, double, bool, CellErr, ExcelDateTime, DateTimeIso>;

std::uint64_t sat_u64(double v) {
    if (!(v > 0)) return 0;
    if (v >= 18446744073709551615.0) return std::numeric_limits<std::uint64_t>::max();
    return static_cast<std::uint64_t>(v);
}

std::int64_t sat_i64(double v) {
    if (std::isnan(v)) return 0;
    if (v >= 9223372036854775807.0) return std::numeric_limits<std::int64_t>::max();
    if (v <= -9223372036854775808.0) return std::numeric_limits<std::int64_t>::min();
    return static_cast<std::int64_t>(v);
}

std::size_t sat_usize(float v) {
    if (!(v > 0)) return 0;
    if (v >= 1.8e19F) return std::numeric_limits<std::size_t>::max();
    return static_cast<std::size_t>(v);
}

// Rust `f64::fract`.
double fract(double v) { return v - std::trunc(v); }

struct Ymd {
    std::uint16_t year;
    std::uint8_t month, day, hour, minute, second;
    std::uint16_t milli;
};

bool is_leap_year(std::uint64_t y) { return y % 4 == 0 && (y % 100 != 0 || y % 400 == 0); }

// calamine `ExcelDateTime::excel_to_standard_datetime`.
Ymd to_ymd_hms_milli(double excel_datetime, bool is_1904) {
    constexpr std::uint64_t YEAR_DAYS = 365, YEAR_DAYS_4 = YEAR_DAYS * 4 + 1, YEAR_DAYS_100 = YEAR_DAYS * 100 + 25,
                            YEAR_DAYS_400 = YEAR_DAYS * 400 + 97;
    constexpr std::uint64_t MAX_SERIAL_DAYS_1900 = 2'958'465, MAX_SERIAL_DAYS_1904 = 2'957'003;
    constexpr double DAY_SECONDS = 24.0 * 60.0 * 60.0;
    constexpr std::uint64_t HOUR_SECONDS = 3600, MINUTE_SECONDS = 60;
    std::uint64_t months[12] = {31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31};

    std::uint64_t days = std::min(sat_u64(std::floor(excel_datetime)), is_1904 ? MAX_SERIAL_DAYS_1904 : MAX_SERIAL_DAYS_1900);
    if (is_1904) days += 111'033;
    else if (days > YEAR_DAYS) days += 109'571;
    else days += 109'572;

    const std::uint64_t year_days_400 = days / YEAR_DAYS_400;
    days %= YEAR_DAYS_400;

    std::uint64_t year_days_100 = 0;
    if (days < YEAR_DAYS_100) {
        year_days_100 = days / YEAR_DAYS_100;
        days %= YEAR_DAYS_100;
    } else {
        year_days_100 = 1 + (days - YEAR_DAYS_100) / (YEAR_DAYS_100 - 1);
        days = (days - YEAR_DAYS_100) % (YEAR_DAYS_100 - 1);
    }

    std::uint64_t year_days_4 = 0;
    bool non_leap_year_block = false;
    if (year_days_100 == 0) {
        year_days_4 = days / YEAR_DAYS_4;
        days %= YEAR_DAYS_4;
    } else if (days < YEAR_DAYS_4) {
        year_days_4 = days / (YEAR_DAYS_4 - 1);
        days %= YEAR_DAYS_4 - 1;
        non_leap_year_block = true;
    } else {
        year_days_4 = 1 + (days - (YEAR_DAYS_4 - 1)) / YEAR_DAYS_4;
        days = (days - (YEAR_DAYS_4 - 1)) % YEAR_DAYS_4;
    }

    std::uint64_t year_days_1 = 0;
    if (non_leap_year_block) {
        year_days_1 = days / YEAR_DAYS;
        days %= YEAR_DAYS;
    } else if (days < YEAR_DAYS + 1) {
        year_days_1 = days / (YEAR_DAYS + 1);
        days %= YEAR_DAYS + 1;
    } else {
        year_days_1 = 1 + (days - (YEAR_DAYS + 1)) / YEAR_DAYS;
        days = (days - (YEAR_DAYS + 1)) % YEAR_DAYS;
    }

    const std::uint64_t year = 1600 + year_days_400 * 400 + year_days_100 * 100 + year_days_4 * 4 + year_days_1;
    days += 1;
    if (is_leap_year(year)) months[1] = 29;
    if (!is_1904 && year == 1900) {
        months[1] = 29;
        if (std::trunc(excel_datetime) == 366.0) days += 1;
    }
    std::uint64_t month = 1;
    for (const std::uint64_t month_days : months) {
        if (days > month_days) {
            days -= month_days;
            month += 1;
        } else {
            break;
        }
    }
    const std::uint64_t day = days;

    const double time = fract(excel_datetime);
    std::uint64_t milli = sat_u64(std::round(fract(time * DAY_SECONDS) * 1000.0));
    std::uint64_t day_as_seconds = sat_u64(time * DAY_SECONDS);
    if (milli == 1000) {
        day_as_seconds += 1;
        milli = 0;
    }
    if (day_as_seconds == static_cast<std::uint64_t>(DAY_SECONDS))
        return to_ymd_hms_milli(std::floor(excel_datetime) + 1.0, is_1904);
    const std::uint64_t hour = day_as_seconds / HOUR_SECONDS;
    const std::uint64_t min = (day_as_seconds - hour * HOUR_SECONDS) / MINUTE_SECONDS;
    const std::uint64_t sec = (day_as_seconds - hour * HOUR_SECONDS - min * MINUTE_SECONDS) % MINUTE_SECONDS;
    return Ymd{static_cast<std::uint16_t>(year), static_cast<std::uint8_t>(month), static_cast<std::uint8_t>(day),
               static_cast<std::uint8_t>(hour),  static_cast<std::uint8_t>(min),   static_cast<std::uint8_t>(sec),
               static_cast<std::uint16_t>(milli)};
}

struct Hms {
    const char* sign;
    std::uint64_t h, m, s, ms;
};

// Sign, hours, minutes, seconds and milliseconds of a span of `ms` milliseconds.
Hms hms(std::int64_t ms) {
    const std::uint64_t abs = ms < 0 ? 0 - static_cast<std::uint64_t>(ms) : static_cast<std::uint64_t>(ms);
    return {ms < 0 ? "-" : "", abs / 3'600'000, abs / 60'000 % 60, abs / 1000 % 60, abs % 1000};
}

std::int64_t saturating_mul(std::int64_t a, std::int64_t b) {
    std::int64_t out = 0;
    if (__builtin_mul_overflow(a, b, &out))
        return (a < 0) != (b < 0) ? std::numeric_limits<std::int64_t>::min() : std::numeric_limits<std::int64_t>::max();
    return out;
}

Cell from_data(const Data& data) {
    struct V {
        Cell operator()(std::monostate) const { return {Cell::Empty{}}; }
        Cell operator()(const Text& s) const { return {Cell::String{s.value}}; }
        Cell operator()(double f) const { return {Cell::Float{f}}; }
        Cell operator()(bool b) const { return {Cell::Bool{b}}; }
        Cell operator()(const CellErr& e) const { return {Cell::Error{e.value}}; }
        Cell operator()(const DateTimeIso& s) const { return {Cell::DateTime{std::nullopt, s.value}}; }
        Cell operator()(const ExcelDateTime& dt) const {
            if (dt.duration) {
                const auto t = hms(sat_i64(std::round(dt.value * 86'400'000.0)));
                const std::string frac = t.ms == 0 ? std::string() : std::format(".{:03}", t.ms);
                return {Cell::Duration{dt.value, std::format("{}PT{}H{}M{}{}S", t.sign, t.h, t.m, t.s, frac)}};
            }
            const auto d = to_ymd_hms_milli(dt.value, dt.is_1904);
            const std::string frac = d.milli == 0 ? std::string() : std::format(".{:03}", d.milli);
            return {Cell::DateTime{dt.value, std::format("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}", d.year, d.month, d.day,
                                                         d.hour, d.minute, d.second, frac)}};
        }
    };
    return std::visit(V{}, data);
}

// The text a cell shows and its alignment, Excel style: numbers and dates
// right, booleans and errors centered, text left.
std::pair<std::string, float> display(const Data& data) {
    struct V {
        std::pair<std::string, float> operator()(std::monostate) const { return {"", LEFT}; }
        std::pair<std::string, float> operator()(const Text& s) const { return {s.value, LEFT}; }
        std::pair<std::string, float> operator()(const DateTimeIso& s) const { return {s.value, RIGHT}; }
        std::pair<std::string, float> operator()(bool b) const { return {b ? "TRUE" : "FALSE", CENTER}; }
        std::pair<std::string, float> operator()(const CellErr& e) const { return {e.value, CENTER}; }
        std::pair<std::string, float> operator()(double f) const { return {xlsx::general(f), RIGHT}; }
        std::pair<std::string, float> operator()(const ExcelDateTime& dt) const {
            if (dt.duration) {
                const auto t = hms(saturating_mul(sat_i64(std::round(dt.value * 86'400.0)), 1000));
                return {std::format("{}{}:{:02}:{:02}", t.sign, t.h, t.m, t.s), RIGHT};
            }
            const auto d = to_ymd_hms_milli(dt.value, dt.is_1904);
            if (dt.value < 1.0) return {std::format("{:02}:{:02}:{:02}", d.hour, d.minute, d.second), RIGHT};
            if (fract(dt.value) == 0.0) return {std::format("{:04}-{:02}-{:02}", d.year, d.month, d.day), RIGHT};
            return {std::format("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", d.year, d.month, d.day, d.hour, d.minute, d.second),
                    RIGHT};
        }
    };
    return std::visit(V{}, data);
}

// ---------------------------------------------------------------------------
// Number formats, as calamine `formats.rs`.

enum class CellFormat : std::uint8_t { Other, DateTime, TimeDelta };

std::vector<char32_t> code_points(std::string_view s) {
    std::vector<char32_t> out;
    out.reserve(s.size());
    for (std::size_t i = 0; i < s.size();) {
        const auto b = static_cast<unsigned char>(s[i]);
        std::size_t n = b < 0x80 ? 1 : b < 0xE0 ? 2 : b < 0xF0 ? 3 : 4;
        n = std::min(n, s.size() - i);
        char32_t c = n == 1 ? b : n == 2 ? (b & 0x1F) : n == 3 ? (b & 0x0F) : (b & 0x07);
        for (std::size_t k = 1; k < n; ++k) c = (c << 6) | (static_cast<unsigned char>(s[i + k]) & 0x3F);
        out.push_back(c);
        i += n;
    }
    return out;
}

bool eq_ignore_ascii_case(char32_t a, char32_t b) {
    auto lower = [](char32_t c) { return c >= 'A' && c <= 'Z' ? c + 32 : c; };
    return lower(a) == lower(b);
}

CellFormat detect_custom_number_format(std::string_view format) {
    bool escaped = false, is_quote = false, hms_ = false, ap = false;
    std::uint8_t brackets = 0;
    char32_t prev = ' ';
    for (const char32_t s : code_points(format)) {
        if (escaped) escaped = false;
        else if (s == '_' || s == '\\' || s == '*') escaped = true;
        else if (s == '"' && is_quote) is_quote = false;
        else if (is_quote) {
        } else if (s == '"') is_quote = true;
        else if (s == ';') return CellFormat::Other;
        else if (s == '[') brackets = static_cast<std::uint8_t>(brackets + 1);
        else if (s == ']' && brackets == 1 && hms_) return CellFormat::TimeDelta;
        else if (s == ']') brackets = brackets == 0 ? 0 : static_cast<std::uint8_t>(brackets - 1);
        else if ((s == 'a' || s == 'A') && !ap && brackets == 0) ap = true;
        else if ((s == 'p' || s == 'm' || s == '/' || s == 'P' || s == 'M') && ap && brackets == 0)
            return CellFormat::DateTime;
        else if (std::u32string_view(U"dmhysDMHYS").find(s) != std::u32string_view::npos && !ap && brackets == 0)
            return CellFormat::DateTime;
        else if (hms_ && eq_ignore_ascii_case(s, prev)) {
        } else {
            hms_ = prev == '[' && std::u32string_view(U"mhsMHS").find(s) != std::u32string_view::npos;
        }
        prev = s;
    }
    return CellFormat::Other;
}

CellFormat builtin_format_by_id(std::string_view id) {
    static constexpr std::string_view dates[] = {"14", "15", "16", "17", "18", "19", "20", "21", "22", "45", "47",
                                                 "27", "28", "29", "30", "31", "32", "33", "34", "35", "36", "50",
                                                 "51", "52", "53", "54", "55", "56", "57", "58"};
    if (std::ranges::find(dates, id) != std::end(dates)) return CellFormat::DateTime;
    if (id == "46") return CellFormat::TimeDelta;
    return CellFormat::Other;
}

// ---------------------------------------------------------------------------
// XML helpers.

enum class Step : std::uint8_t { Skip, Descend, Stop };

// Visits the nodes below `root` in document order; `leave` runs after the
// children of every node `enter` descended into. Returns false when stopped.
template <typename Enter, typename Leave>
bool walk(const pugi::xml_node& root, Enter&& enter, Leave&& leave) {
    pugi::xml_node n = root.first_child();
    while (n) {
        const Step step = enter(n);
        if (step == Step::Stop) return false;
        if (step == Step::Descend && n.first_child()) {
            n = n.first_child();
            continue;
        }
        if (step == Step::Descend) leave(n);
        while (n != root && !n.next_sibling()) {
            n = n.parent();
            if (n != root) leave(n);
        }
        if (n == root) break;
        n = n.next_sibling();
    }
    return true;
}

template <typename Enter>
bool walk(const pugi::xml_node& root, Enter&& enter) {
    return walk(root, std::forward<Enter>(enter), [](const pugi::xml_node&) {});
}

bool is_element(const pugi::xml_node& n) { return n.type() == pugi::node_element; }

// All text below `node`, as quick-xml's text events up to its end tag.
std::string text_of(const pugi::xml_node& node) {
    std::string out;
    walk(node, [&](const pugi::xml_node& n) {
        if (n.type() == pugi::node_pcdata || n.type() == pugi::node_cdata) out += n.value();
        return Step::Descend;
    });
    return out;
}

// An attribute by its exact (prefixed) name.
std::optional<std::string_view> raw_attr(const pugi::xml_node& n, const char* name) {
    const auto a = n.attribute(name);
    if (!a) return std::nullopt;
    return std::string_view(a.value());
}

// `atoi_simd::parse::<usize>`: ASCII digits only.
std::optional<std::size_t> parse_usize(std::string_view s) {
    if (s.empty()) return std::nullopt;
    std::size_t v = 0;
    const auto [p, ec] = std::from_chars(s.data(), s.data() + s.size(), v);
    if (ec != std::errc() || p != s.data() + s.size()) return std::nullopt;
    return v;
}

// calamine `unescape_xml`: Excel's `_xHHHH_` UTF-16 escapes.
std::optional<std::uint16_t> parse_xml_escape(std::string_view s, std::size_t i) {
    if (i + 7 > s.size() || s[i] != '_' || s[i + 1] != 'x' || s[i + 6] != '_') return std::nullopt;
    std::uint16_t v = 0;
    for (std::size_t k = 2; k < 6; ++k) {
        const char c = s[i + k];
        int d = 0;
        if (c >= '0' && c <= '9') d = c - '0';
        else if (c >= 'a' && c <= 'f') d = c - 'a' + 10;
        else if (c >= 'A' && c <= 'F') d = c - 'A' + 10;
        else return std::nullopt;
        v = static_cast<std::uint16_t>(v * 16 + d);
    }
    return v;
}

void push_utf8(std::string& out, char32_t c) {
    if (c > 0x10FFFF || (c >= 0xD800 && c <= 0xDFFF)) c = 0xFFFD;
    if (c < 0x80) out += static_cast<char>(c);
    else if (c < 0x800) {
        out += static_cast<char>(0xC0 | (c >> 6));
        out += static_cast<char>(0x80 | (c & 0x3F));
    } else if (c < 0x10000) {
        out += static_cast<char>(0xE0 | (c >> 12));
        out += static_cast<char>(0x80 | ((c >> 6) & 0x3F));
        out += static_cast<char>(0x80 | (c & 0x3F));
    } else {
        out += static_cast<char>(0xF0 | (c >> 18));
        out += static_cast<char>(0x80 | ((c >> 12) & 0x3F));
        out += static_cast<char>(0x80 | ((c >> 6) & 0x3F));
        out += static_cast<char>(0x80 | (c & 0x3F));
    }
}

std::string unescape_xml(std::string_view s) {
    std::string out;
    out.reserve(s.size());
    std::size_t i = 0;
    while (i < s.size()) {
        const auto unit = s[i] == '_' ? parse_xml_escape(s, i) : std::nullopt;
        if (!unit) {
            out += s[i++];
            continue;
        }
        i += 7;
        if (*unit >= 0xD800 && *unit <= 0xDBFF) {
            const auto low = parse_xml_escape(s, i);
            if (low && *low >= 0xDC00 && *low <= 0xDFFF) {
                push_utf8(out, 0x10000 + ((char32_t{*unit} - 0xD800) << 10) + (char32_t{*low} - 0xDC00));
                i += 7;
            } else {
                push_utf8(out, 0xFFFD);
            }
        } else {
            push_utf8(out, *unit);
        }
    }
    return out;
}

// calamine `read_string_with_bufs`: plain or rich text, phonetic runs skipped.
std::string read_string(const pugi::xml_node& node) {
    std::optional<std::string> rich;
    bool phonetic = false;
    walk(
        node,
        [&](const pugi::xml_node& e) {
            if (!is_element(e)) return Step::Descend;
            const auto name = ooxml::local_name(e);
            if (name == "r" && !rich) {
                rich = std::string();
            } else if (name == "rPh") {
                phonetic = true;
            } else if (name == "t" && !phonetic) {
                const bool preserve = raw_attr(e, "xml:space") == "preserve";
                std::string value = text_of(e);
                std::string_view v = value;
                if (!preserve) {
                    constexpr std::string_view ws = " \t\r\n";
                    const auto a = v.find_first_not_of(ws);
                    v = a == std::string_view::npos ? std::string_view() : v.substr(a, v.find_last_not_of(ws) - a + 1);
                }
                std::string out = unescape_xml(v);
                if (rich) *rich += out;
                else rich = std::move(out);
                return Step::Skip;
            }
            return Step::Descend;
        },
        [&](const pugi::xml_node& e) {
            if (ooxml::local_name(e) == "rPh") phonetic = false;
        });
    return rich.value_or(std::string());
}

// ---------------------------------------------------------------------------
// Cell references, as calamine's `get_row_and_optional_column` and `Reference`.

Result<std::pair<std::uint32_t, std::optional<std::uint32_t>>> row_and_optional_column(std::string_view range) {
    std::size_t i = 0;
    std::uint32_t col = 0, row = 0;
    auto alnum = [](unsigned char c) { return bad(std::format("Expecting alphanumeric character, got {:X}", c)); };
    while (i < range.size()) {
        const auto c = static_cast<unsigned char>(range[i]);
        if (c >= 'A' && c <= 'Z') col = col * 26 + (c - 'A') + 1;
        else if (c >= 'a' && c <= 'z') col = col * 26 + (c - 'a') + 1;
        else if (c >= '0' && c <= '9') {
            row = c - '0';
            ++i;
            break;
        } else return std::unexpected(alnum(c));
        ++i;
    }
    for (; i < range.size(); ++i) {
        const auto c = static_cast<unsigned char>(range[i]);
        if (c < '0' || c > '9') return std::unexpected(alnum(c));
        row = row * 10 + (c - '0');
    }
    if (row == 0) return std::unexpected(bad("Range is missing the expected row component."));
    return std::pair{row - 1, col == 0 ? std::nullopt : std::optional<std::uint32_t>(col - 1)};
}

Result<Pos> get_row_column(std::string_view range) {
    auto rc = row_and_optional_column(range);
    if (!rc) return std::unexpected(std::move(rc.error()));
    if (!rc->second) return std::unexpected(bad("Range is missing the expected column component."));
    return Pos{rc->first, *rc->second};
}

Result<Area> get_dimension(std::string_view dimension) {
    std::vector<Pos> parts;
    std::size_t from = 0;
    while (true) {
        const auto colon = dimension.find(':', from);
        auto p = get_row_column(dimension.substr(from, colon == std::string_view::npos ? colon : colon - from));
        if (!p) return std::unexpected(std::move(p.error()));
        parts.push_back(*p);
        if (colon == std::string_view::npos) break;
        from = colon + 1;
    }
    if (parts.size() == 1) return Area{parts[0], parts[0]};
    if (parts.size() == 2) return Area{parts[0], parts[1]};
    return std::unexpected(bad(std::format("Range dimension must be lower than 2. Got {}", parts.size())));
}

struct Reference {
    enum class Kind : std::uint8_t { Cell, Row, Column } kind;
    std::uint32_t row = 0, col = 0;
    bool absolute_row = false, absolute_col = false;

    [[nodiscard]] bool valid() const {
        if (kind != Kind::Row && col >= MAX_COLUMNS) return false;
        if (kind != Kind::Column && row >= MAX_ROWS) return false;
        return true;
    }

    static std::optional<Reference> parse(std::string_view name) {
        std::uint32_t col = 0, row = 0;
        bool absolute_col = false, absolute_row = false;
        auto alpha = [](char c) { return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z'); };
        auto digit = [](char c) { return c >= '0' && c <= '9'; };
        for (std::size_t i = 0; i < name.size(); ++i) {
            const char c = name[i];
            const bool has_next = i + 1 < name.size();
            if (c == '$' && has_next && alpha(name[i + 1])) {
                if (row > 0 || col > 0) return std::nullopt;
                absolute_col = true;
            } else if (c == '$' && has_next && digit(name[i + 1])) {
                if (row > 0) return std::nullopt;
                absolute_row = true;
            } else if (c == '$') {
                return std::nullopt;
            } else if (alpha(c)) {
                if (row > 0) return std::nullopt;
                const char upper = c >= 'a' ? static_cast<char>(c - 32) : c;
                col = col * 26 + static_cast<std::uint32_t>(upper - 'A') + 1;
            } else if (digit(c)) {
                row = row * 10 + static_cast<std::uint32_t>(c - '0');
            } else {
                return std::nullopt;
            }
        }
        Reference r{Kind::Cell, row == 0 ? 0 : row - 1, col == 0 ? 0 : col - 1, absolute_row, absolute_col};
        if (col > 0 && row > 0) r.kind = Kind::Cell;
        else if (col > 0) r.kind = Kind::Column;
        else if (row > 0) r.kind = Kind::Row;
        else return std::nullopt;
        if (!r.valid()) return std::nullopt;
        return r;
    }

    [[nodiscard]] std::optional<Reference> offset(std::int64_t rows, std::int64_t cols) const {
        Reference r = *this;
        if (kind != Kind::Row && !absolute_col) r.col = static_cast<std::uint32_t>(static_cast<std::int64_t>(col) + cols);
        if (kind != Kind::Column && !absolute_row) r.row = static_cast<std::uint32_t>(static_cast<std::int64_t>(row) + rows);
        if (!r.valid()) return std::nullopt;
        return r;
    }

    void format(std::string& out) const {
        if (kind != Kind::Row) {
            if (absolute_col) out += '$';
            out += xlsx::column_name(col);
        }
        if (kind != Kind::Column) {
            if (absolute_row) out += '$';
            out += std::to_string(std::uint64_t{row} + 1);
        }
    }
};

// Shifts a cell or range reference token, none when it is not one or leaves the sheet.
std::optional<std::string> offset_range(std::string_view range, std::int64_t rows, std::int64_t cols) {
    std::string out;
    const auto colon = range.find(':');
    if (colon == std::string_view::npos) {
        const auto r = Reference::parse(range);
        if (!r || r->kind != Reference::Kind::Cell) return std::nullopt;
        const auto o = r->offset(rows, cols);
        if (!o) return std::nullopt;
        o->format(out);
        return out;
    }
    const auto a = Reference::parse(range.substr(0, colon));
    if (!a) return std::nullopt;
    const auto b = Reference::parse(range.substr(colon + 1));
    if (!b || a->kind != b->kind) return std::nullopt;
    const auto oa = a->offset(rows, cols), ob = b->offset(rows, cols);
    if (!oa || !ob) return std::nullopt;
    oa->format(out);
    out += ':';
    ob->format(out);
    return out;
}

// calamine `expand_shared_formula`.
std::string expand_shared_formula(std::string_view formula, Pos start, Pos end) {
    const std::int64_t rows = static_cast<std::int64_t>(end.first) - start.first;
    const std::int64_t cols = static_cast<std::int64_t>(end.second) - start.second;
    std::string res;
    res.reserve(formula.size());
    bool in_quote = false;
    std::size_t token_start = 0, token_end = 0;
    auto token = [&](bool next_is_paren) {
        const auto t = formula.substr(token_start, token_end - token_start);
        const auto shifted = next_is_paren ? std::nullopt : offset_range(t, rows, cols);
        res += shifted ? std::string_view(*shifted) : t;
    };
    for (std::size_t i = 0; i < formula.size(); ++i) {
        const auto c = static_cast<unsigned char>(formula[i]);
        const bool alnum = (c >= '0' && c <= '9') || (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z');
        if (!in_quote && (alnum || c == '\\' || c == '.' || c == '_' || c == '$' || c == ':')) {
            token_end = i + 1;
        } else {
            if (token_start < token_end) token(c == '(');
            res += static_cast<char>(c);
            token_start = token_end = i + 1;
            if (c == '"') in_quote = !in_quote;
        }
    }
    if (token_start < token_end) token(false);
    return res;
}

// ---------------------------------------------------------------------------
// The workbook reader.

struct Sheet {
    std::string name;
    std::unordered_map<std::uint64_t, Data> values;
    // Formula text without the leading `=`, only where a cell has one.
    std::unordered_map<std::uint64_t, std::string> formulas;
    // Bounding box of the non-empty values and formulas.
    std::optional<Area> used;

    [[nodiscard]] const Data& value(std::uint32_t row, std::uint32_t col) const {
        static const Data empty;
        const auto it = values.find(key({row, col}));
        return it == values.end() ? empty : it->second;
    }

    [[nodiscard]] std::optional<std::string> formula(std::uint32_t row, std::uint32_t col) const {
        const auto it = formulas.find(key({row, col}));
        if (it == formulas.end()) return std::nullopt;
        return it->second;
    }

    // Rows and columns drawn, from A1 to the end of the used range, capped.
    [[nodiscard]] std::pair<std::uint32_t, std::uint32_t> render_extent() const {
        if (!used) return {1, 1};
        return {std::min(used->end.first + 1, xlsx::RENDER_ROWS), std::min(used->end.second + 1, xlsx::RENDER_COLUMNS)};
    }
};

using Doc = std::unique_ptr<pugi::xml_document>;

Result<std::optional<Doc>> load(ooxml::Package& package, const std::string& path) {
    auto bytes = package.read(path);
    if (!bytes) return std::unexpected(std::move(bytes.error()));
    if (!*bytes) return std::optional<Doc>();
    const auto& b = **bytes;
    auto doc = ooxml::xml_reader(std::string_view(reinterpret_cast<const char*>(b.data()), b.size()));
    if (!doc) return std::unexpected(bad("Xml error: " + doc.error()));
    return std::optional<Doc>(std::move(*doc));
}

class Reader {
  public:
    explicit Reader(ooxml::Package package) : package_(std::move(package)) {}

    Result<std::vector<Sheet>> read() {
        if (auto r = read_package_relationships(); !r) return std::unexpected(std::move(r.error()));
        if (auto r = read_shared_strings(); !r) return std::unexpected(std::move(r.error()));
        if (auto r = read_styles(); !r) return std::unexpected(std::move(r.error()));
        auto rels = read_relationships();
        if (!rels) return std::unexpected(std::move(rels.error()));
        if (auto r = read_workbook(*rels); !r) return std::unexpected(std::move(r.error()));
        std::vector<Sheet> sheets;
        sheets.reserve(worksheets_.size());
        for (const auto& [name, path] : worksheets_) {
            auto sheet = read_sheet(name, path);
            if (!sheet) return std::unexpected(std::move(sheet.error()));
            sheets.push_back(std::move(*sheet));
        }
        return sheets;
    }

  private:
    using Rels = std::unordered_map<std::string, std::pair<std::string, std::string>>;

    Result<void> read_package_relationships() {
        auto xml = load(package_, "_rels/.rels");
        if (!xml) return std::unexpected(std::move(xml.error()));
        if (!*xml) return std::unexpected(bad("File not found '_rels/.rels'"));
        std::optional<std::string> target;
        walk(***xml, [&](const pugi::xml_node& e) {
            if (is_element(e) && ooxml::local_name(e) == "Relationship") {
                const auto type = raw_attr(e, "Type");
                const auto t = raw_attr(e, "Target");
                if (type && type->ends_with("/relationships/officeDocument") && t) target = std::string(*t);
            }
            return Step::Descend;
        });
        if (!target) return std::unexpected(bad("Relationship not found"));
        const auto slash = target->rfind('/');
        std::string_view dir = slash == std::string::npos ? std::string_view() : std::string_view(*target).substr(0, slash + 1);
        const std::string_view name =
            slash == std::string::npos ? std::string_view(*target) : std::string_view(*target).substr(slash + 1);
        if (dir.starts_with('/')) dir.remove_prefix(1);
        xl_path_ = std::string(dir);
        if (!name.empty()) workbook_name_ = std::string(name);
        return {};
    }

    Result<void> read_shared_strings() {
        auto xml = load(package_, xl_path_ + "sharedStrings.xml");
        if (!xml) return std::unexpected(std::move(xml.error()));
        if (!*xml) return {};
        walk(***xml, [&](const pugi::xml_node& e) {
            if (!is_element(e) || ooxml::local_name(e) != "si") return Step::Descend;
            strings_.push_back(read_string(e));
            return Step::Skip;
        });
        return {};
    }

    Result<void> read_styles() {
        auto xml = load(package_, xl_path_ + "styles.xml");
        if (!xml) return std::unexpected(std::move(xml.error()));
        if (!*xml) return {};
        std::unordered_map<std::string, std::string> number_formats;
        int in_num_fmts = 0, in_cell_xfs = 0;
        walk(
            ***xml,
            [&](const pugi::xml_node& e) {
                if (!is_element(e)) return Step::Descend;
                const auto name = ooxml::local_name(e);
                if (name == "numFmts") ++in_num_fmts;
                else if (name == "cellXfs") ++in_cell_xfs;
                else if (name == "numFmt" && in_num_fmts > 0) {
                    const auto id = raw_attr(e, "numFmtId");
                    const auto code = raw_attr(e, "formatCode");
                    if (id && code) number_formats.insert_or_assign(std::string(*id), std::string(*code));
                } else if (name == "xf" && in_cell_xfs > 0) {
                    const auto id = raw_attr(e, "numFmtId");
                    if (!id) {
                        formats_.push_back(CellFormat::Other);
                    } else if (const auto it = number_formats.find(std::string(*id)); it != number_formats.end()) {
                        formats_.push_back(detect_custom_number_format(it->second));
                    } else {
                        formats_.push_back(builtin_format_by_id(*id));
                    }
                    return Step::Skip;
                }
                return Step::Descend;
            },
            [&](const pugi::xml_node& e) {
                const auto name = ooxml::local_name(e);
                if (name == "numFmts") --in_num_fmts;
                else if (name == "cellXfs") --in_cell_xfs;
            });
        return {};
    }

    Result<Rels> read_relationships() {
        const std::string path = xl_path_ + "_rels/" + workbook_name_ + ".rels";
        auto xml = load(package_, path);
        if (!xml) return std::unexpected(std::move(xml.error()));
        if (!*xml) return std::unexpected(bad(std::format("File not found '{}'", path)));
        Rels rels;
        walk(***xml, [&](const pugi::xml_node& e) {
            if (is_element(e) && ooxml::local_name(e) == "Relationship") {
                if (const auto id = raw_attr(e, "Id")) {
                    rels.insert_or_assign(std::string(*id), std::pair{std::string(raw_attr(e, "Target").value_or("")),
                                                                      std::string(raw_attr(e, "Type").value_or(""))});
                }
            }
            return Step::Descend;
        });
        return rels;
    }

    Result<void> read_workbook(const Rels& rels) {
        auto xml = load(package_, xl_path_ + workbook_name_);
        if (!xml) return std::unexpected(std::move(xml.error()));
        if (!*xml) return {};
        std::optional<DocsError> error;
        walk(***xml, [&](const pugi::xml_node& e) {
            if (!is_element(e)) return Step::Descend;
            const auto local = ooxml::local_name(e);
            if (local == "workbookPr") {
                if (const auto v = raw_attr(e, "date1904")) is_1904_ = *v == "1" || *v == "true";
                return Step::Descend;
            }
            if (local != "sheet") return Step::Descend;
            std::string name;
            std::string_view rel_id;
            for (const auto& a : e.attributes()) {
                const std::string_view k = a.name();
                if (k == "name") {
                    name = a.value();
                } else if (k == "state") {
                    const std::string_view v = a.value();
                    if (v != "visible" && v != "hidden" && v != "veryHidden") {
                        error = bad(std::format("Unrecognized sheet:state: {}", v));
                        return Step::Stop;
                    }
                } else if (k == "id" || k.ends_with(":id")) {
                    rel_id = a.value();
                }
            }
            const auto it = rels.find(std::string(rel_id));
            if (it == rels.end()) {
                error = bad("Relationship not found");
                return Step::Stop;
            }
            const auto& [target, rel_type] = it->second;
            std::string path = target.starts_with('/') ? target.substr(1) : xl_path_ + target;
            const auto slash = rel_type.rfind('/');
            const std::string_view typ =
                slash == std::string::npos ? std::string_view(rel_type) : std::string_view(rel_type).substr(slash + 1);
            if (typ != "worksheet" && typ != "chartsheet" && typ != "dialogsheet") {
                error = bad(std::format("Unrecognized sheet:type: {}", path));
                return Step::Stop;
            }
            if (typ == "worksheet") worksheets_.emplace_back(std::move(name), std::move(path));
            return Step::Descend;
        });
        if (error) return std::unexpected(std::move(*error));
        return {};
    }

    // Walks the cells of `sheet_data` with calamine's row and column tracking.
    template <typename OnCell>
    Result<void> each_cell(const pugi::xml_node& sheet_data, OnCell&& on_cell) {
        std::uint32_t row_index = 0, col_index = 0;
        std::optional<DocsError> error;
        walk(
            sheet_data,
            [&](const pugi::xml_node& e) {
                if (!is_element(e)) return Step::Descend;
                const auto local = ooxml::local_name(e);
                if (local == "row") {
                    if (const auto r = raw_attr(e, "r")) {
                        auto row = row_and_optional_column(*r);
                        if (!row) {
                            error = std::move(row.error());
                            return Step::Stop;
                        }
                        row_index = row->first;
                    }
                    return Step::Descend;
                }
                if (local != "c") return Step::Descend;
                Pos pos{row_index, col_index};
                if (const auto r = raw_attr(e, "r")) {
                    auto p = get_row_column(*r);
                    if (!p) {
                        error = std::move(p.error());
                        return Step::Stop;
                    }
                    pos = *p;
                    col_index = p->second;
                }
                if (auto r = on_cell(e, pos); !r) {
                    error = std::move(r.error());
                    return Step::Stop;
                }
                col_index += 1;
                return Step::Skip;
            },
            [&](const pugi::xml_node& e) {
                if (ooxml::local_name(e) == "row") {
                    row_index += 1;
                    col_index = 0;
                }
            });
        if (error) return std::unexpected(std::move(*error));
        return {};
    }

    Result<Data> read_v(std::string_view v, std::optional<std::string_view> style, std::optional<std::string_view> type) const {
        std::optional<CellFormat> format = CellFormat::Other;
        if (style) {
            const std::size_t id = parse_usize(*style).value_or(0);
            format = id < formats_.size() ? std::optional(formats_[id]) : std::nullopt;
        }
        if (type == "s") {
            if (v.empty()) return Data{};
            const std::size_t idx = parse_usize(v).value_or(0);
            if (idx >= strings_.size()) return std::unexpected(bad("Cell string index not found in shared strings table"));
            return Data{Text{strings_[idx]}};
        }
        if (type == "b") {
            constexpr std::string_view ws = " \t\r\n\f\v";
            const auto a = v.find_first_not_of(ws);
            const std::string_view t = a == std::string_view::npos ? std::string_view() : v.substr(a, v.find_last_not_of(ws) - a + 1);
            auto ieq = [&](std::string_view w) {
                return t.size() == w.size() &&
                       std::ranges::equal(t, w, [](char x, char y) { return eq_ignore_ascii_case(char32_t(x), char32_t(y)); });
            };
            if (t.empty()) return Data{};
            if (t == "0" || ieq("false")) return Data{false};
            if (t == "1" || ieq("true")) return Data{true};
            return std::unexpected(bad(std::format("Unrecognized boolean cell value: {}", v)));
        }
        if (type == "d") return Data{DateTimeIso{std::string(v)}};
        if (type == "e") {
            static constexpr std::string_view errors[] = {"#DIV/0!", "#N/A", "#NAME?", "#NULL!", "#NUM!", "#REF!", "#VALUE!"};
            if (std::ranges::find(errors, v) == std::end(errors))
                return std::unexpected(bad(std::format("Unsupported cell error value '{}'", v)));
            return Data{CellErr{std::string(v)}};
        }
        if (type == "str") return Data{Text{unescape_xml(v)}};
        if (!type || type == "n") {
            if (v.empty()) return Data{};
            std::string_view digits = v;
            if (digits.starts_with('+') && digits.size() > 1 && digits[1] != '-' && digits[1] != '+') digits.remove_prefix(1);
            double n = 0;
            const auto [p, ec] = std::from_chars(digits.data(), digits.data() + digits.size(), n);
            if (ec == std::errc() && p == digits.data() + digits.size()) {
                if (format == CellFormat::DateTime) return Data{ExcelDateTime{n, false, is_1904_}};
                if (format == CellFormat::TimeDelta) return Data{ExcelDateTime{n, true, is_1904_}};
                return Data{n};
            }
            if (!type) return Data{Text{std::string(v)}};
            return std::unexpected(bad("Parse float error: invalid float literal"));
        }
        return std::unexpected(bad(std::format("Unknown cell 't' attribute: \"{}\"", *type)));
    }

    Result<Data> read_value(const pugi::xml_node& e, std::optional<std::string_view> style,
                            std::optional<std::string_view> type) const {
        const auto name = ooxml::local_name(e);
        if (name == "is") return Data{Text{read_string(e)}};
        if (name == "v") {
            if (type == "inlineStr" || type == "is") return Data{};
            if (!type || type == "n" || type == "s" || type == "b" || type == "e") {
                // calamine parses the first text event only.
                const auto first = e.first_child();
                if (!first || first.type() != pugi::node_pcdata) return Data{};
                return read_v(first.value(), style, type);
            }
            return read_v(text_of(e), style, type);
        }
        if (name == "f") return Data{};
        return std::unexpected(bad("Expecting 'v, f, or is' node"));
    }

    struct SharedFormula {
        std::string formula;
        Area range;
    };

    // calamine `read_formula_record` with shared formulas expanded.
    static Result<std::optional<std::string>> read_formula(const pugi::xml_node& e, Pos pos,
                                                           std::unordered_map<std::size_t, SharedFormula>& shared) {
        const auto name = ooxml::local_name(e);
        std::optional<std::string> formula;
        if (name == "f") formula = text_of(e);
        else if (name != "is" && name != "v") return std::unexpected(bad("Expecting 'v, f, or is' node"));
        if (raw_attr(e, "t") != "shared") return formula;
        const auto si = raw_attr(e, "si");
        if (!si) return std::unexpected(bad("si attribute is mandatory if it is shared"));
        const auto index = parse_usize(*si);
        if (!index) return std::unexpected(bad("si attribute must be a number"));
        if (const auto ref = raw_attr(e, "ref")) {
            auto range = get_dimension(*ref);
            if (!range) return std::unexpected(std::move(range.error()));
            std::string text = formula.value_or(std::string());
            shared.insert_or_assign(*index, SharedFormula{text, *range});
            return std::optional(std::move(text));
        }
        const auto it = shared.find(*index);
        if (it == shared.end()) return std::optional<std::string>();
        return std::optional(expand_shared_formula(it->second.formula, it->second.range.start, pos));
    }

    Result<Sheet> read_sheet(const std::string& name, const std::string& path) {
        auto xml = load(package_, path);
        if (!xml) return std::unexpected(std::move(xml.error()));
        if (!*xml) return std::unexpected(bad(std::format("Worksheet '{}' not found", name)));
        Sheet sheet{name, {}, {}, std::nullopt};

        // Elements before `sheetData`: the dimension and the root's kind.
        pugi::xml_node sheet_data;
        std::optional<std::string> sh_type;
        std::optional<DocsError> error;
        walk(***xml, [&](const pugi::xml_node& e) {
            if (!is_element(e)) return Step::Descend;
            const auto local = ooxml::local_name(e);
            if (local == "dimension") {
                const auto ref = raw_attr(e, "ref");
                if (!ref) {
                    error = bad("Expecting 'dimension' node");
                    return Step::Stop;
                }
                if (auto d = get_dimension(*ref); !d) {
                    error = std::move(d.error());
                    return Step::Stop;
                }
            } else if (local == "sheetData") {
                sheet_data = e;
                return Step::Stop;
            } else if (!sh_type) {
                sh_type = std::string(local);
            }
            return Step::Descend;
        });
        if (error) return std::unexpected(std::move(*error));
        if (!sheet_data) {
            if (sh_type) return sheet; // not a worksheet: calamine reads it as empty
            return std::unexpected(bad("Unexpected end of xml, expecting '</worksheet>'"));
        }

        auto values = each_cell(sheet_data, [&](const pugi::xml_node& c, Pos pos) -> Result<void> {
            const auto style = raw_attr(c, "s");
            const auto type = raw_attr(c, "t");
            Data value;
            for (const auto& child : c.children()) {
                if (!is_element(child)) continue;
                auto v = read_value(child, style, type);
                if (!v) return std::unexpected(std::move(v.error()));
                value = std::move(*v);
            }
            if (!std::holds_alternative<std::monostate>(value)) sheet.values.insert_or_assign(key(pos), std::move(value));
            return {};
        });
        if (!values) return std::unexpected(std::move(values.error()));

        std::unordered_map<std::size_t, SharedFormula> shared;
        auto formulas = each_cell(sheet_data, [&](const pugi::xml_node& c, Pos pos) -> Result<void> {
            std::optional<std::string> value;
            for (const auto& child : c.children()) {
                if (!is_element(child)) continue;
                auto f = read_formula(child, pos, shared);
                if (!f) return std::unexpected(std::move(f.error()));
                if (*f) value = std::move(**f);
            }
            if (value && !value->empty()) sheet.formulas.insert_or_assign(key(pos), std::move(*value));
            return {};
        });
        if (!formulas) return std::unexpected(std::move(formulas.error()));

        auto include = [&](std::uint64_t k) {
            const Pos p{static_cast<std::uint32_t>(k >> 32), static_cast<std::uint32_t>(k)};
            if (!sheet.used) {
                sheet.used = Area{p, p};
                return;
            }
            auto& u = *sheet.used;
            u.start = {std::min(u.start.first, p.first), std::min(u.start.second, p.second)};
            u.end = {std::max(u.end.first, p.first), std::max(u.end.second, p.second)};
        };
        for (const auto& [k, _] : sheet.values) include(k);
        for (const auto& [k, _] : sheet.formulas) include(k);
        return sheet;
    }

    ooxml::Package package_;
    std::string xl_path_ = "xl/";
    std::string workbook_name_ = "workbook.xml";
    std::vector<std::string> strings_;
    std::vector<CellFormat> formats_;
    bool is_1904_ = false;
    std::vector<std::pair<std::string, std::string>> worksheets_;
};

// A compound file (encrypted OOXML) holding an `EncryptedPackage` stream.
bool password_protected(std::span<const std::uint8_t> bytes) {
    static constexpr std::uint8_t magic[] = {0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1};
    if (bytes.size() < sizeof magic || !std::equal(std::begin(magic), std::end(magic), bytes.begin())) return false;
    std::string name;
    for (const char c : std::string_view("EncryptedPackage")) {
        name += c;
        name += '\0';
    }
    const std::string_view hay(reinterpret_cast<const char*>(bytes.data()), bytes.size());
    return hay.find(name) != std::string_view::npos;
}

// ---------------------------------------------------------------------------
// Ranges in A1 notation.

Area include(Area area, Pos p) {
    return {{std::min(area.start.first, p.first), std::min(area.start.second, p.second)},
            {std::max(area.end.first, p.first), std::max(area.end.second, p.second)}};
}

// A1 notation: `B3` for one cell, `B3:D5` otherwise.
std::string area_name(Area area) {
    std::string start = xlsx::address(area.start.first, area.start.second);
    if (area.start == area.end) return start;
    return start + ":" + xlsx::address(area.end.first, area.end.second);
}

bool is_space(char c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v'; }

// Parses an A1 cell reference (`$` markers allowed, case insensitive).
Result<Pos> parse_cell(std::string_view text) {
    std::string_view s = text;
    while (!s.empty() && is_space(s.front())) s.remove_prefix(1);
    while (!s.empty() && is_space(s.back())) s.remove_suffix(1);
    if (s.starts_with('$')) s.remove_prefix(1);
    std::size_t n = 0;
    auto alpha = [](char c) { return (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z'); };
    while (n < s.size() && alpha(s[n])) ++n;
    const std::string_view letters = s.substr(0, n);
    std::string_view digits = s.substr(n);
    if (digits.starts_with('$')) digits.remove_prefix(1);
    std::uint64_t col = 0;
    for (const char c : letters) {
        const char upper = c >= 'a' ? static_cast<char>(c - 32) : c;
        col = std::min<std::uint64_t>(col * 26 + static_cast<std::uint64_t>(upper - 'A') + 1, 0xFFFF'FFFF);
    }
    std::uint32_t row = 0;
    const auto [p, ec] = std::from_chars(digits.data(), digits.data() + digits.size(), row);
    const bool ok = !digits.empty() && ec == std::errc() && p == digits.data() + digits.size();
    if (ok && row >= 1 && row <= MAX_ROWS && col >= 1 && col <= MAX_COLUMNS)
        return Pos{row - 1, static_cast<std::uint32_t>(col - 1)};
    return std::unexpected(DocsError::request(std::format("`{}` is not a cell in A1 notation such as B3", text)));
}

// Parses `B2:D5` or `C3` into a normalized area.
Result<Area> parse_range(std::string_view range) {
    const auto colon = range.find(':');
    const std::string_view a_text = colon == std::string_view::npos ? range : range.substr(0, colon);
    const std::string_view b_text = colon == std::string_view::npos ? range : range.substr(colon + 1);
    auto a = parse_cell(a_text);
    if (!a) return std::unexpected(std::move(a.error()));
    auto b = parse_cell(b_text);
    if (!b) return std::unexpected(std::move(b.error()));
    return include(Area{*a, *a}, *b);
}

std::string ascii_lower(std::string_view s) {
    std::string out(s);
    for (char& c : out)
        if (c >= 'A' && c <= 'Z') c = static_cast<char>(c + 32);
    return out;
}

// The longest prefix of `text` no wider than `max` pixels.
template <typename WidthOf>
std::string_view fit(std::string_view text, float max, const WidthOf& width_of) {
    if (width_of(text) <= max) return text;
    float width = 0;
    for (std::size_t i = 0; i < text.size();) {
        const auto b = static_cast<unsigned char>(text[i]);
        const std::size_t n = std::min<std::size_t>(b < 0x80 ? 1 : b < 0xE0 ? 2 : b < 0xF0 ? 3 : 4, text.size() - i);
        width += width_of(text.substr(i, n));
        if (width > max) return text.substr(0, i);
        i += n;
    }
    return text;
}

json::Value opt_string(const std::optional<std::string>& s) { return s ? json::Value(*s) : json::Value(nullptr); }
json::Value opt_double(std::optional<double> v) { return v ? json::Value(*v) : json::Value(nullptr); }

} // namespace

// ---------------------------------------------------------------------------

namespace xlsx {

std::string column_name(std::uint32_t col) {
    const char letter = static_cast<char>('A' + col % 26);
    if (col < 26) return std::string(1, letter);
    return column_name(col / 26 - 1) + letter;
}

std::string address(std::uint32_t row, std::uint32_t col) { return column_name(col) + std::to_string(std::uint64_t{row} + 1); }

std::string general(double v) {
    if (std::isfinite(v) && fract(v) == 0.0 && std::abs(v) < 1e15) return std::format("{:.0f}", v);
    if (std::isfinite(v) && v != 0.0 && (std::abs(v) >= 1e15 || std::abs(v) < 1e-4)) {
        // Rust `{:.4e}`: no `+` and no zero padding in the exponent.
        const std::string c = std::format("{:.4e}", v);
        const auto e = c.find('e');
        std::string_view exp = std::string_view(c).substr(e + 1);
        const bool negative = exp.starts_with('-');
        if (negative || exp.starts_with('+')) exp.remove_prefix(1);
        while (exp.size() > 1 && exp.front() == '0') exp.remove_prefix(1);
        return c.substr(0, e + 1) + (negative ? "-" : "") + std::string(exp);
    }
    if (std::isnan(v)) return "NaN";
    if (std::isinf(v)) return v > 0 ? "inf" : "-inf";
    std::string text = std::format("{:.9f}", v);
    if (text.find('.') != std::string::npos) {
        while (text.ends_with('0')) text.pop_back();
        if (text.ends_with('.')) text.pop_back();
    }
    return text;
}

} // namespace xlsx

json::Value SheetInfo::to_json() const {
    return json::Object{{"name", name},
                        {"rows", std::uint64_t{rows}},
                        {"columns", std::uint64_t{columns}},
                        {"start", opt_string(start)}};
}

json::Value Cell::to_json() const {
    struct V {
        json::Value operator()(const Empty&) const { return json::Object{{"type", "empty"}}; }
        json::Value operator()(const String& s) const { return json::Object{{"type", "string"}, {"value", s.value}}; }
        json::Value operator()(const Float& f) const { return json::Object{{"type", "float"}, {"value", f.value}}; }
        json::Value operator()(const Int& i) const { return json::Object{{"type", "int"}, {"value", i.value}}; }
        json::Value operator()(const Bool& b) const { return json::Object{{"type", "bool"}, {"value", b.value}}; }
        json::Value operator()(const Error& e) const { return json::Object{{"type", "error"}, {"value", e.value}}; }
        json::Value operator()(const DateTime& d) const {
            return json::Object{{"type", "date_time"},
                                {"value", json::Object{{"serial", opt_double(d.serial)}, {"iso", d.iso}}}};
        }
        json::Value operator()(const Duration& d) const {
            return json::Object{{"type", "duration"},
                                {"value", json::Object{{"serial", opt_double(d.serial)}, {"iso", d.iso}}}};
        }
    };
    return std::visit(V{}, v);
}

json::Value CellOut::to_json() const {
    return json::Object{{"address", address}, {"value", value.to_json()}, {"formula", opt_string(formula)}};
}

json::Value CellBlock::to_json() const {
    json::Array out;
    out.reserve(rows.size());
    for (const auto& row : rows) {
        json::Array cells;
        cells.reserve(row.size());
        for (const auto& c : row) cells.push_back(c.to_json());
        out.emplace_back(std::move(cells));
    }
    return json::Object{{"sheet", sheet}, {"range", range}, {"rows", std::move(out)}};
}

struct XlsxDoc::Impl {
    std::vector<Sheet> sheets;

    // The sheet called `name` (exact match first, then ignoring case), or the first sheet.
    [[nodiscard]] Result<const Sheet*> sheet(std::optional<std::string_view> name) const {
        if (!name) {
            if (sheets.empty()) return std::unexpected(DocsError::request("the workbook has no sheets"));
            return &sheets.front();
        }
        for (const auto& s : sheets)
            if (s.name == *name) return &s;
        const std::string lower = ascii_lower(*name);
        for (const auto& s : sheets)
            if (ascii_lower(s.name) == lower) return &s;
        std::string names;
        for (const auto& s : sheets) {
            if (!names.empty()) names += ", ";
            names += s.name;
        }
        return std::unexpected(DocsError::request(std::format("no sheet named `{}`, the sheets are: {}", *name, names)));
    }
};

XlsxDoc::XlsxDoc(std::unique_ptr<Impl> impl) : impl_(std::move(impl)) {}
XlsxDoc::~XlsxDoc() = default;

Result<std::unique_ptr<XlsxDoc>> XlsxDoc::parse(std::span<const std::uint8_t> bytes) {
    try {
        if (password_protected(bytes)) return std::unexpected(bad("Workbook is password protected"));
        auto package = ooxml::Package::open(bytes, Format::Xlsx);
        if (!package) {
            // calamine wraps zip failures as `Zip error: ...`.
            std::string message = package.error().message();
            const std::string prefix = std::format("invalid {} file: ", to_string(Format::Xlsx));
            if (message.starts_with(prefix)) message.erase(0, prefix.size());
            return std::unexpected(bad("Zip error: " + message));
        }
        Reader reader(std::move(*package));
        auto sheets = reader.read();
        if (!sheets) return std::unexpected(std::move(sheets.error()));
        auto impl = std::make_unique<Impl>();
        impl->sheets = std::move(*sheets);
        return std::unique_ptr<XlsxDoc>(new XlsxDoc(std::move(impl)));
    } catch (...) {
        return std::unexpected(bad("malformed document part"));
    }
}

std::vector<SheetInfo> XlsxDoc::sheets() const {
    std::vector<SheetInfo> out;
    out.reserve(impl_->sheets.size());
    for (const auto& s : impl_->sheets) {
        SheetInfo info{s.name, 0, 0, std::nullopt};
        if (s.used) {
            info.rows = s.used->end.first - s.used->start.first + 1;
            info.columns = s.used->end.second - s.used->start.second + 1;
            info.start = xlsx::address(s.used->start.first, s.used->start.second);
        }
        out.push_back(std::move(info));
    }
    return out;
}

Result<CellBlock> XlsxDoc::cells(std::optional<std::string_view> sheet_name, std::string_view range) const {
    try {
        auto sheet = impl_->sheet(sheet_name);
        if (!sheet) return std::unexpected(std::move(sheet.error()));
        const Sheet& s = **sheet;
        auto parsed = parse_range(range);
        if (!parsed) return std::unexpected(std::move(parsed.error()));
        Area area = *parsed;
        const Pos last = s.used ? s.used->end : area.start;
        area.end = {std::max(std::min(area.end.first, last.first), area.start.first),
                    std::max(std::min(area.end.second, last.second), area.start.second)};
        if (area.len() > MAX_BLOCK_CELLS) {
            return std::unexpected(DocsError::request(std::format("range {} has {} cells, request at most {} at a time",
                                                                  area_name(area), area.len(), MAX_BLOCK_CELLS)));
        }
        CellBlock block{s.name, area_name(area), {}};
        for (std::uint32_t row = area.start.first; row <= area.end.first; ++row) {
            std::vector<CellOut> cells;
            for (std::uint32_t col = area.start.second; col <= area.end.second; ++col)
                cells.push_back(CellOut{xlsx::address(row, col), from_data(s.value(row, col)), s.formula(row, col)});
            block.rows.push_back(std::move(cells));
        }
        return block;
    } catch (...) {
        return std::unexpected(bad("malformed document part"));
    }
}

std::size_t XlsxDoc::page_count() const { return impl_->sheets.size(); }

Result<PageSize> XlsxDoc::page_size(std::size_t index) const {
    if (index >= impl_->sheets.size()) return std::unexpected(DocsError::page_range(index, impl_->sheets.size()));
    const auto [rows, cols] = impl_->sheets[index].render_extent();
    return PageSize{xlsx::ROW_HEADER_WIDTH + static_cast<float>(cols) * xlsx::COLUMN_WIDTH,
                    static_cast<float>(rows + 1) * xlsx::ROW_HEIGHT};
}

std::optional<Result<json::Value>> XlsxDoc::query(const DataQuery& query) const {
    if (std::holds_alternative<query::Sheets>(query.v)) {
        json::Array out;
        for (const auto& s : sheets()) out.push_back(s.to_json());
        return Result<json::Value>(json::Value(std::move(out)));
    }
    if (const auto* c = std::get_if<query::Cells>(&query.v)) {
        auto block = cells(c->sheet ? std::optional<std::string_view>(*c->sheet) : std::nullopt, c->range);
        if (!block) return Result<json::Value>(std::unexpected(std::move(block.error())));
        return Result<json::Value>(block->to_json());
    }
    return std::nullopt;
}

Result<Rgba> XlsxDoc::render(std::size_t index, float scale, const Fonts& fonts, const RenderOpts& opts) const {
    auto size = page_size(index);
    if (!size) return std::unexpected(std::move(size.error()));
    try {
        const Sheet& sheet = impl_->sheets[index];
        const auto [rows, cols] = sheet.render_extent();
        const auto [width, height] = size->pixels(scale);
        const float w = static_cast<float>(width), h = static_cast<float>(height);
        const float col_w = xlsx::COLUMN_WIDTH * scale, row_h = xlsx::ROW_HEIGHT * scale,
                    head_w = xlsx::ROW_HEADER_WIDTH * scale, pad = xlsx::CELL_PADDING * scale;
        const auto& palette = opts.palette;
        const TextStyle style = opts.text(opts.typography.cell * scale);
        TextStyle header_style = style;
        header_style.color = palette.header_text;
        const float text_top = (row_h - fonts.line_height(style.size)) / 2.0F;
        const auto width_of = [&](std::string_view text) { return fonts.text_width(text, style.size, false); };
        Canvas canvas(width, height, palette.page);

        canvas.fill_rect(0.0F, 0.0F, w, row_h, palette.header);
        canvas.fill_rect(0.0F, 0.0F, head_w, h, palette.header);
        for (std::uint32_t c = 1; c <= cols; ++c)
            canvas.fill_rect(std::min(head_w + static_cast<float>(c) * col_w, w - 1.0F), 0.0F, 1.0F, h, palette.grid);
        for (std::uint32_t r = 1; r <= rows; ++r)
            canvas.fill_rect(0.0F, std::min(static_cast<float>(r + 1) * row_h, h - 1.0F), w, 1.0F, palette.grid);
        canvas.fill_rect(head_w, 0.0F, 1.0F, h, palette.rule);
        canvas.fill_rect(0.0F, row_h, w, 1.0F, palette.rule);

        for (std::uint32_t c = 0; c < cols; ++c) {
            const std::string name = xlsx::column_name(c);
            const float x = head_w + static_cast<float>(c) * col_w + (col_w - width_of(name)) / 2.0F;
            canvas.draw_text(fonts, header_style, x, text_top, name);
        }
        const float avail = col_w - 2.0F * pad;
        for (std::uint32_t r = 0; r < rows; ++r) {
            const float y = static_cast<float>(r + 1) * row_h + text_top;
            const std::string number = std::to_string(std::uint64_t{r} + 1);
            canvas.draw_text(fonts, header_style, (head_w - width_of(number)) / 2.0F, y, number);
            for (std::uint32_t c = 0; c < cols; ++c) {
                const auto [full, align] = display(sheet.value(r, c));
                std::string text;
                if (align == RIGHT && width_of(full) > avail) {
                    text.assign(std::min(sat_usize(avail / std::max(width_of("#"), 1.0F)), MAX_HASHES), '#');
                } else {
                    const std::string_view first = std::string_view(full).substr(0, std::string_view(full).find('\n'));
                    text = std::string(fit(first, avail, width_of));
                }
                const float x = head_w + static_cast<float>(c) * col_w + pad + (avail - width_of(text)) * align;
                canvas.draw_text(fonts, style, x, y, text);
            }
        }
        return std::move(canvas).into_rgba();
    } catch (...) {
        return std::unexpected(bad("malformed document part"));
    }
}

} // namespace loa::surfaces::docs
