#include <doctest/doctest.h>

#include "../kit/support.hpp"
#include "loa/harness/account_support/account_support.hpp"
#include "loa/harness/account_support/configdoc.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa::harness::account_support;
using Kind = loa::settings::models::Error::Kind;
using loa::harness::test::golden;
using loa::harness::test::read_file;

TEST_CASE("account_support: JSON edits preserve key order and reject non-object parents") {
    auto doc = JsonDoc::parse(R"({"z":1,"nested":{"b":2,"a":"old"},"a":3})", "settings").value();
    REQUIRE(doc.set({"nested", "a"}, "new"));
    REQUIRE(doc.set({"nested", "b"}, std::nullopt));
    REQUIRE(doc.set({"nested", "c"}, "last"));
    CHECK(doc.render() == "{\n  \"z\": 1,\n  \"nested\": {\n    \"a\": \"new\",\n    \"c\": \"last\"\n  },\n  \"a\": 3\n}\n");
    auto bad = doc.set({"z", "child"}, "x");
    REQUIRE_FALSE(bad);
    CHECK(bad.error().kind == Kind::Unsupported);
    CHECK(bad.error().to_string().find("\"z\" is not an object") != std::string::npos);
    CHECK(doc.get({"nested", "a"}) == "new");
    CHECK(JsonDoc::parse("  ", "x").value().render() == "{}\n");
    CHECK(JsonDoc::parse("[1]", "x").error().kind == Kind::Unsupported);
    CHECK(JsonDoc::parse("{", "x").error().kind == Kind::Parse);
    CHECK(doc.set(KeyPath{}, "x").error().kind == Kind::Invalid);
}

TEST_CASE("account_support: TOML edits keep comments and support nested paths") {
    auto doc = TomlDoc::parse("# header\n[model]\nname = 'old' # note\n", "config").value();
    REQUIRE(doc.set({"model", "name"}, "new"));
    CHECK(doc.get({"model", "name"}) == "new");
    CHECK(doc.render() == "# header\n[model]\nname = \"new\" # note\n");
    auto bad = doc.set({"model", "name", "x"}, "v");
    REQUIRE_FALSE(bad);
    CHECK(bad.error().kind == Kind::Unsupported);
    CHECK(doc.set({"model", "name", "x"}, std::nullopt));
    CHECK(TomlDoc::parse("a = [", "x").error().kind == Kind::Parse);
}

TEST_CASE("account_support: config document goldens match the Rust editors byte for byte") {
    auto json = JsonDoc::parse(read_file(golden("harness/configdoc_in.json")), "settings").value();
    const std::vector<std::pair<std::vector<std::string_view>, std::optional<std::string_view>>> json_steps{
        {{"nested", "a"}, "new"}, {{"nested", "b"}, std::nullopt}, {{"nested", "c"}, "last"},
        {{"new", "deep", "k"}, "v"}, {{"missing", "x"}, std::nullopt}, {{"z", "x"}, std::nullopt},
        {{"s"}, "\xC3\xA9/<>\"\n\x01"}};
    for (const auto& [path, value] : json_steps) REQUIRE(json.set(KeyPath(path), value));
    CHECK(json.render() == read_file(golden("harness/configdoc_out.json")));

    auto toml = TomlDoc::parse(read_file(golden("harness/configdoc_in.toml")), "config").value();
    const std::vector<std::pair<std::vector<std::string_view>, std::optional<std::string_view>>> toml_steps{
        {{"model"}, "gpt-5"},
        {{"profiles", "fast", "model_reasoning_effort"}, "high"},
        {{"other", "name"}, "new"},
        {{"approval"}, std::nullopt},
        {{"dotted", "key"}, "d2"},
        {{"features", "web", "mode"}, "on"},
        {{"profiles", "fast", "quote"}, "a\"b"},
        {{"top"}, "t"},
        {{"other", "x"}, std::nullopt},
        {{"missing", "k"}, std::nullopt},
        {{"other", "esc"}, "tab\there\x7f"},
    };
    for (const auto& [path, value] : toml_steps) {
        INFO(path.front());
        REQUIRE(toml.set(KeyPath(path), value));
    }
    CHECK(toml.render() == read_file(golden("harness/configdoc_out.toml")));
    // Untouched input lines survive verbatim.
    CHECK(toml.render().find("arr = [\n  \"a\", # c\n  \"b\",\n]\n") != std::string::npos);
}

TEST_CASE("account_support: edit_file saves only real changes") {
    auto dir = loa::runtime::TempDir::create().value();
    const auto path = dir.path() / "settings.json";
    {
        std::ofstream(path) << "{\"model\":\"a\"}";
    }
    auto snapshot = loa::runtime::fs::Snapshot::load(path).value();
    auto same = edit_file<JsonDoc>(snapshot, "{\"model\":\"a\"}", [](JsonDoc& d) { return d.set({"model"}, "a"); });
    REQUIRE(same);
    CHECK_FALSE(*same);
    auto changed = edit_file<JsonDoc>(snapshot, "{\"model\":\"a\"}", [](JsonDoc& d) { return d.set({"model"}, "b"); });
    REQUIRE(changed);
    CHECK(*changed);
    CHECK(read_file(path) == "{\n  \"model\": \"b\"\n}\n");
}

TEST_CASE("account_support: validate_model_name") {
    CHECK(validate_model_name("model", "gpt-5"));
    CHECK(validate_model_name("model", "").error().to_string().find("model name is empty") != std::string::npos);
    CHECK(validate_model_name("model", "a b").error().kind == Kind::Invalid);
    CHECK_FALSE(validate_model_name("model", "a\xC2\xA0" "b"));
    CHECK_FALSE(validate_model_name("model", "a\x01"));
}
