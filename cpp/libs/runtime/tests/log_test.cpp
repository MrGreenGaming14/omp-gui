#include <doctest/doctest.h>

#include "loa/runtime/log.hpp"

using namespace loa::runtime::log;

TEST_CASE("LOA_LOG parses a default level plus target=level overrides") {
    auto f = Filter::parse("warn, loa::net=trace,loa::net::quic=error ,loa::ui=DEBUG").value();
    CHECK(f.default_level == Level::warn);
    REQUIRE(f.overrides.size() == 3);
    CHECK(f.level_for("loa::app") == Level::warn);
    CHECK(f.level_for("loa::net") == Level::trace);
    CHECK(f.level_for("loa::net::http") == Level::trace);
    CHECK(f.level_for("loa::net::quic::conn") == Level::error);
    CHECK(f.level_for("loa::network") == Level::warn); // not a `::` child
    CHECK(f.level_for("loa::ui") == Level::debug);
    CHECK(f.enabled(Level::info, "loa::ui"));
    CHECK_FALSE(f.enabled(Level::info, "loa::app"));
}

TEST_CASE("empty spec defaults to error; bad specs fail") {
    auto f = Filter::parse("").value();
    CHECK(f.default_level == Level::error);
    CHECK(f.overrides.empty());
    CHECK_FALSE(Filter::parse("loud"));
    CHECK_FALSE(Filter::parse("a=loud"));
    CHECK_FALSE(Filter::parse("=info"));
    CHECK(Filter::parse("off").value().enabled(Level::error, "x") == false);
}

TEST_CASE("global filter") {
    set_filter(Filter::parse("info,quiet=off").value());
    CHECK(enabled(Level::info, "loa"));
    CHECK_FALSE(enabled(Level::debug, "loa"));
    CHECK_FALSE(enabled(Level::error, "quiet"));
    set_filter({});
}
