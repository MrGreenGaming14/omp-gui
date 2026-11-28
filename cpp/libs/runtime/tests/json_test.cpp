#include <doctest/doctest.h>

#include "loa/runtime/json.hpp"

namespace json = loa::runtime::json;

TEST_CASE("object key order survives parse then dump") {
    const std::string text = R"({"zeta":1,"alpha":{"y":true,"b":null,"a":[3,2,1]},"mid":"s","beta":-2.5})";
    auto v = json::parse(text);
    REQUIRE(v);
    CHECK(json::dump(*v) == text);
    const auto* obj = v->as_object();
    REQUIRE(obj);
    CHECK((*obj)[0].first == "zeta");
    CHECK((*obj)[3].first == "beta");
}

TEST_CASE("compact dump strips whitespace and escapes") {
    auto v = json::parse(" { \"a\" : [ 1 , \"x\\ny\" ] } ");
    REQUIRE(v);
    CHECK(json::dump(*v) == R"({"a":[1,"x\ny"]})");
}

TEST_CASE("numbers keep their kind") {
    auto v = json::parse(R"([18446744073709551615,-9223372036854775808,1.5,7])").value();
    const auto& a = *v.as_array();
    CHECK(a[0].as_u64() == 18446744073709551615ULL);
    CHECK_FALSE(a[0].as_i64());
    CHECK(a[1].as_i64() == INT64_MIN);
    CHECK(a[2].as_f64() == 1.5);
    CHECK(a[3].as_i64() == 7);
    CHECK(json::dump(v) == "[18446744073709551615,-9223372036854775808,1.5,7]");
}

TEST_CASE("build and set preserve insertion position") {
    json::Value v = json::Object{};
    v.set("b", 1);
    v.set("a", "x");
    v.set("b", json::Array{true, nullptr});
    CHECK(json::dump(v) == R"({"b":[true,null],"a":"x"})");
    CHECK(v.find("a")->as_string()->compare("x") == 0);
    CHECK(v.find("missing") == nullptr);
}

TEST_CASE("invalid json is an error") {
    auto v = json::parse("{\"a\":");
    REQUIRE_FALSE(v);
    CHECK(v.error().code() == loa::ErrorCode::parse);
}
