#include "loa/runtime/time.hpp"

#include <algorithm>
#include <chrono>
#include <cstdio>

namespace loa::runtime::time {

namespace {

std::chrono::nanoseconds since_epoch() noexcept {
    const auto d = std::chrono::system_clock::now().time_since_epoch();
    return std::max(std::chrono::duration_cast<std::chrono::nanoseconds>(d), std::chrono::nanoseconds::zero());
}

// Howard Hinnant's days_from_civil / civil_from_days.
constexpr std::int64_t days_from_civil(std::int64_t y, unsigned m, unsigned d) noexcept {
    y -= m <= 2;
    const std::int64_t era = (y >= 0 ? y : y - 399) / 400;
    const auto yoe = static_cast<unsigned>(y - era * 400);
    const unsigned doy = (153 * (m > 2 ? m - 3 : m + 9) + 2) / 5 + d - 1;
    const unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    return era * 146097 + static_cast<std::int64_t>(doe) - 719468;
}

struct Civil {
    std::int64_t y;
    unsigned m, d;
};

constexpr Civil civil_from_days(std::int64_t z) noexcept {
    z += 719468;
    const std::int64_t era = (z >= 0 ? z : z - 146096) / 146097;
    const auto doe = static_cast<unsigned>(z - era * 146097);
    const unsigned yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    const unsigned doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    const unsigned mp = (5 * doy + 2) / 153;
    const unsigned d = doy - (153 * mp + 2) / 5 + 1;
    const unsigned m = mp < 10 ? mp + 3 : mp - 9;
    return {static_cast<std::int64_t>(yoe) + era * 400 + (m <= 2), m, d};
}

constexpr unsigned days_in_month(std::int64_t y, unsigned m) noexcept {
    constexpr unsigned table[] = {31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31};
    const bool leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    return m == 2 && leap ? 29 : table[m - 1];
}

constexpr std::int64_t floor_div(std::int64_t a, std::int64_t b) noexcept { return a / b - ((a % b != 0) && ((a < 0) != (b < 0))); }

struct Cursor {
    std::string_view s;
    std::size_t i = 0;

    bool digits(int n, int& out) noexcept {
        out = 0;
        for (int k = 0; k < n; ++k) {
            if (i >= s.size() || s[i] < '0' || s[i] > '9') return false;
            out = out * 10 + (s[i++] - '0');
        }
        return true;
    }
    bool lit(char c) noexcept {
        if (i < s.size() && s[i] == c) {
            ++i;
            return true;
        }
        return false;
    }
};

} // namespace

std::int64_t now_ms() noexcept { return std::chrono::duration_cast<std::chrono::milliseconds>(since_epoch()).count(); }
std::uint64_t now_secs() noexcept {
    return static_cast<std::uint64_t>(std::chrono::duration_cast<std::chrono::seconds>(since_epoch()).count());
}
std::uint64_t now_us() noexcept {
    return static_cast<std::uint64_t>(std::chrono::duration_cast<std::chrono::microseconds>(since_epoch()).count());
}

std::string ago(std::int64_t now_ms, std::int64_t then_ms) {
    const auto minutes = std::max<std::int64_t>(now_ms - then_ms, 0) / 60'000;
    if (minutes == 0) return "just now";
    if (minutes < 60) return std::to_string(minutes) + "m ago";
    if (minutes < 1440) return std::to_string(minutes / 60) + "h ago";
    return std::to_string(minutes / 1440) + "d ago";
}

std::string until(std::int64_t ms) {
    const auto minutes = std::max<std::int64_t>(ms / 60'000, 0);
    const auto days = minutes / 1440, hours = (minutes / 60) % 24, mins = minutes % 60;
    if (days == 0 && hours == 0) return std::to_string(mins) + "m";
    if (days == 0) return std::to_string(hours) + "h " + std::to_string(mins) + "m";
    return std::to_string(days) + "d " + std::to_string(hours) + "h";
}

// Follows the `time` crate's `Rfc3339` parser: any one-byte date/time separator,
// any number of fraction digits, `Z`/`z` or `±HH:MM`, a leap second allowed.
std::optional<std::int64_t> parse_iso_ms(std::string_view s) noexcept {
    Cursor c{s};
    int year = 0, month = 0, day = 0, hour = 0, minute = 0, second = 0;
    if (!c.digits(4, year) || !c.lit('-') || !c.digits(2, month) || !c.lit('-') || !c.digits(2, day)) return std::nullopt;
    if (c.i >= s.size()) return std::nullopt;
    ++c.i;
    if (!c.digits(2, hour) || !c.lit(':') || !c.digits(2, minute) || !c.lit(':') || !c.digits(2, second)) return std::nullopt;
    std::int64_t nanos = 0;
    if (c.lit('.')) {
        std::int64_t mult = 100'000'000;
        bool any = false;
        while (c.i < s.size() && s[c.i] >= '0' && s[c.i] <= '9') {
            nanos += (s[c.i++] - '0') * mult;
            mult /= 10;
            any = true;
        }
        if (!any) return std::nullopt;
    }
    int offset_minutes = 0;
    if (c.lit('Z') || c.lit('z')) {
    } else {
        int sign = 0;
        if (c.lit('+')) sign = 1;
        else if (c.lit('-')) sign = -1;
        else return std::nullopt;
        int oh = 0, om = 0;
        if (!c.digits(2, oh) || oh > 23 || !c.lit(':') || !c.digits(2, om) || om > 59) return std::nullopt;
        offset_minutes = sign * (oh * 60 + om);
    }
    if (c.i != s.size()) return std::nullopt;
    if (month < 1 || month > 12 || day < 1 || static_cast<unsigned>(day) > days_in_month(year, static_cast<unsigned>(month)) ||
        hour > 23 || minute > 59 || second > 60)
        return std::nullopt;
    if (second == 60) {
        // A leap second reads as the last nanosecond of the minute before it, and only at 23:59 UTC.
        const int utc = ((hour * 60 + minute - offset_minutes) % 1440 + 1440) % 1440;
        if (utc != 23 * 60 + 59) return std::nullopt;
        second = 59;
        nanos = 999'999'999;
    }
    const auto days = days_from_civil(year, static_cast<unsigned>(month), static_cast<unsigned>(day));
    const auto secs = days * 86'400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    const auto total_ns = static_cast<__int128>(secs) * 1'000'000'000 + nanos;
    return static_cast<std::int64_t>(total_ns / 1'000'000);
}

std::string format_iso_ms(std::int64_t ms) {
    const auto days = floor_div(ms, 86'400'000);
    const auto rem = ms - days * 86'400'000;
    const auto civil = civil_from_days(days);
    if (civil.y < -9999 || civil.y > 9999) return {};
    const auto h = rem / 3'600'000, m = rem / 60'000 % 60, sec = rem / 1000 % 60, milli = rem % 1000;
    char buf[40];
    std::snprintf(buf, sizeof buf, "%s%04lld-%02u-%02uT%02lld:%02lld:%02lld.%03lldZ", civil.y < 0 ? "-" : "",
                  static_cast<long long>(civil.y < 0 ? -civil.y : civil.y), civil.m, civil.d, static_cast<long long>(h),
                  static_cast<long long>(m), static_cast<long long>(sec), static_cast<long long>(milli));
    return buf;
}

} // namespace loa::runtime::time
