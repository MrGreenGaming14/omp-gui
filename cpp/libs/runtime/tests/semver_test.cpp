#include <doctest/doctest.h>

#include <algorithm>
#include <vector>

#include "loa/runtime/semver.hpp"

using loa::runtime::semver::Version;

namespace {
Version v(std::string_view s) { return Version::parse(s).value(); }
} // namespace

TEST_CASE("pairs from the loa-app / loa-updater tests") {
    CHECK(v("1.2.0") < v("1.10.0"));
    CHECK(v("19.0.0") < v("19.1.0"));
    CHECK(v("19.1.0").to_string() == "19.1.0");
    CHECK(v("19.0.0").to_string() == "19.0.0");
    std::vector<Version> vs{v("1.10.0"), v("1.2.0")};
    std::ranges::sort(vs);
    CHECK(vs.front().to_string() == "1.2.0");
}

TEST_CASE("SemVer 2.0 precedence chain") {
    const std::vector<std::string_view> chain{"1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta",
                                              "1.0.0-beta.2", "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "2.0.0", "2.1.0", "2.1.1"};
    for (std::size_t i = 0; i + 1 < chain.size(); ++i) CHECK_MESSAGE(v(chain[i]) < v(chain[i + 1]), chain[i]);
}

TEST_CASE("build metadata: ignored by precedence, ordered by Ord like semver 1.x") {
    CHECK(v("1.0.0+a").cmp_precedence(v("1.0.0+b")) == 0);
    CHECK(v("1.0.0") < v("1.0.0+a"));
    CHECK(v("1.0.0+a") < v("1.0.0+b"));
    CHECK(v("1.0.0+2") < v("1.0.0+10"));
    CHECK(v("1.0.0+9") < v("1.0.0+a"));
    CHECK(v("1.0.0+a") != v("1.0.0+b"));
}

TEST_CASE("to_string round-trips") {
    for (auto s : {"0.0.0", "1.2.3-rc.1+build.5", "10.20.30+meta", "1.0.0-x-y-z.--"}) CHECK(v(s).to_string() == s);
}

TEST_CASE("strict parsing like the semver crate") {
    for (auto bad : {"", "1", "1.2", "1.2.3.4", "v1.2.3", "01.2.3", "1.02.3", " 1.2.3", "1.2.3-", "1.2.3+", "1.2.3-01",
                     "1.2.3-a..b", "1.2.3-a_b", "1.2.-3", "18446744073709551616.0.0"})
        CHECK_MESSAGE(!Version::parse(bad), bad);
    CHECK(Version::parse("1.2.3+001")); // leading zeros allowed in build metadata
    CHECK(v("18446744073709551615.0.0").major == UINT64_MAX);
}
