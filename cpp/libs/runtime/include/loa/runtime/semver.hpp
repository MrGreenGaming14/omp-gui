#pragma once

#include <compare>
#include <cstdint>
#include <expected>
#include <string>
#include <string_view>

#include "loa/runtime/error.hpp"

namespace loa::runtime::semver {

// `MAJOR.MINOR.PATCH[-pre][+build]`, parsed as strictly as the `semver` 1.x crate.
struct Version {
    std::uint64_t major = 0;
    std::uint64_t minor = 0;
    std::uint64_t patch = 0;
    std::string pre;   // without the leading '-'
    std::string build; // without the leading '+'

    [[nodiscard]] static std::expected<Version, loa::Error> parse(std::string_view text);
    [[nodiscard]] std::string to_string() const;

    // SemVer 2.0 precedence: build metadata ignored.
    [[nodiscard]] std::strong_ordering cmp_precedence(const Version& other) const noexcept;
    // Total order of `semver::Version`'s `Ord`: precedence, then build metadata.
    friend std::strong_ordering operator<=>(const Version& a, const Version& b) noexcept;
    friend bool operator==(const Version& a, const Version& b) noexcept = default;
};

} // namespace loa::runtime::semver
