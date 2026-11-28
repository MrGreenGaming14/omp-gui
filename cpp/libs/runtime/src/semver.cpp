#include "loa/runtime/semver.hpp"

#include <algorithm>
#include <charconv>
#include <optional>
#include <tuple>

namespace loa::runtime::semver {
namespace {

std::unexpected<loa::Error> fail(std::string_view text, std::string_view why) {
    return std::unexpected(loa::Error(ErrorCode::parse, "invalid semver '" + std::string(text) + "': " + std::string(why)));
}

bool all_digits(std::string_view s) { return !s.empty() && std::ranges::all_of(s, [](char c) { return c >= '0' && c <= '9'; }); }

std::optional<std::uint64_t> number(std::string_view s) {
    if (!all_digits(s) || (s.size() > 1 && s[0] == '0')) return std::nullopt;
    std::uint64_t v = 0;
    auto [p, ec] = std::from_chars(s.data(), s.data() + s.size(), v);
    if (ec != std::errc{} || p != s.data() + s.size()) return std::nullopt;
    return v;
}

// Dot-separated identifiers of [0-9A-Za-z-], none empty.
bool valid_idents(std::string_view s, bool no_leading_zero) {
    if (s.empty()) return false;
    for (;;) {
        const auto dot = s.find('.');
        const std::string_view id = s.substr(0, dot);
        if (id.empty()) return false;
        for (char c : id)
            if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '-')) return false;
        if (no_leading_zero && all_digits(id) && id.size() > 1 && id[0] == '0') return false;
        if (dot == std::string_view::npos) return true;
        s.remove_prefix(dot + 1);
    }
}

// Compares two digit strings by numeric value without overflow.
std::strong_ordering cmp_numeric(std::string_view a, std::string_view b) {
    while (a.size() > 1 && a[0] == '0') a.remove_prefix(1);
    while (b.size() > 1 && b[0] == '0') b.remove_prefix(1);
    if (auto c = a.size() <=> b.size(); c != 0) return c;
    return a.compare(b) <=> 0;
}

// Identifier list order shared by prerelease and build metadata in semver 1.x:
// numeric < alphanumeric, numeric by value (then by length for leading zeros),
// alphanumeric lexically, and a shorter prefix sorts first.
std::strong_ordering cmp_idents(std::string_view a, std::string_view b) {
    for (;;) {
        if (a.empty() || b.empty()) return !a.empty() <=> !b.empty();
        const auto da = a.find('.'), db = b.find('.');
        const std::string_view ia = a.substr(0, da), ib = b.substr(0, db);
        const bool na = all_digits(ia), nb = all_digits(ib);
        std::strong_ordering c = std::strong_ordering::equal;
        if (na && nb) {
            c = cmp_numeric(ia, ib);
            if (c == 0) c = ia.size() <=> ib.size();
        } else if (na != nb) {
            c = na ? std::strong_ordering::less : std::strong_ordering::greater;
        } else {
            c = ia.compare(ib) <=> 0;
        }
        if (c != 0) return c;
        a = da == std::string_view::npos ? std::string_view{} : a.substr(da + 1);
        b = db == std::string_view::npos ? std::string_view{} : b.substr(db + 1);
    }
}

} // namespace

std::expected<Version, loa::Error> Version::parse(std::string_view text) {
    std::string_view rest = text;
    Version v;
    if (const auto plus = rest.find('+'); plus != std::string_view::npos) {
        v.build = rest.substr(plus + 1);
        rest = rest.substr(0, plus);
        if (!valid_idents(v.build, false)) return fail(text, "bad build metadata");
    }
    if (const auto dash = rest.find('-'); dash != std::string_view::npos) {
        v.pre = rest.substr(dash + 1);
        rest = rest.substr(0, dash);
        if (!valid_idents(v.pre, true)) return fail(text, "bad pre-release");
    }
    std::uint64_t* parts[] = {&v.major, &v.minor, &v.patch};
    for (int i = 0; i < 3; ++i) {
        const auto dot = rest.find('.');
        if ((i < 2) == (dot == std::string_view::npos)) return fail(text, "expected MAJOR.MINOR.PATCH");
        const auto n = number(rest.substr(0, dot));
        if (!n) return fail(text, "bad version number");
        *parts[i] = *n;
        rest = i < 2 ? rest.substr(dot + 1) : std::string_view{};
    }
    return v;
}

std::string Version::to_string() const {
    std::string out = std::to_string(major) + '.' + std::to_string(minor) + '.' + std::to_string(patch);
    if (!pre.empty()) out += '-' + pre;
    if (!build.empty()) out += '+' + build;
    return out;
}

std::strong_ordering Version::cmp_precedence(const Version& o) const noexcept {
    if (auto c = std::tie(major, minor, patch) <=> std::tie(o.major, o.minor, o.patch); c != 0) return c;
    // A release outranks any pre-release of the same triple.
    if (pre.empty() != o.pre.empty()) return pre.empty() ? std::strong_ordering::greater : std::strong_ordering::less;
    if (pre.empty()) return std::strong_ordering::equal;
    return cmp_idents(pre, o.pre);
}

std::strong_ordering operator<=>(const Version& a, const Version& b) noexcept {
    if (auto c = a.cmp_precedence(b); c != 0) return c;
    return cmp_idents(a.build, b.build);
}

} // namespace loa::runtime::semver
