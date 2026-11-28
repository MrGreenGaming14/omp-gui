#include <doctest/doctest.h>

#include "loa/harness/opencode/opencode.hpp"

namespace {
using loa::runtime::json::Array;
using loa::runtime::json::Object;
using loa::runtime::json::Value;
}

TEST_CASE("opencode provider models retain provider/model ids and names") {
    Value providers(Object{{"providers", Array{
        Value(Object{{"id", "alpha"}, {"models", Object{
            {"key", Value(Object{{"id", "real"}, {"name", "Friendly"}})},
            {"other", Value(Object{})}}}}),
        Value(Object{{"id", "beta"}, {"models", Object{{"one", Value(Object{{"name", "One"}})}}}})
    }}});
    auto models = loa::harness::opencode::provider_models(providers);
    REQUIRE(models.size() == 3);
    CHECK(models[0].id == "alpha/real");
    CHECK(models[0].name == "Friendly");
    CHECK(models[0].provider == "alpha");
    CHECK(models[0].efforts.empty());
    CHECK(models[1].id == "alpha/other");
    CHECK(models[1].name == "other");
    CHECK(models[2].id == "beta/one");
    CHECK(models[2].name == "One");
    CHECK(loa::harness::opencode::provider_models(Value(Object{})).empty());
}
