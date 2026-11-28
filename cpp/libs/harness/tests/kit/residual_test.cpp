#include <doctest/doctest.h>

#include "loa/harness/kit/residual.hpp"
#include "support.hpp"

using namespace loa::harness::kit;
namespace json = loa::runtime::json;
using loa::core::Value;

TEST_CASE("kit: residual attr names and join_parts layout match the Rust golden") {
    const std::string_view parts[] = {"first", "", "trois é", "4"};
    auto [joined, lengths] = residual::join_parts(parts, "\n\n");
    json::Array lens;
    for (const auto& v : lengths) lens.emplace_back(*v.as_int());
    const json::Value doc = json::Object{{"attr", residual::attr_name("claude-code")}, {"joined", joined}, {"lengths", lens}};
    CHECK(json::dump(doc) == loa::harness::test::read_file(loa::harness::test::golden("harness/residual.json")));

    auto split = residual::split_parts(joined, "\n\n", lengths);
    REQUIRE(split);
    CHECK(*split == std::vector<std::string_view>(std::begin(parts), std::end(parts)));
}

TEST_CASE("kit: split_parts refuses text edited since join_parts") {
    const std::string_view parts[] = {"ab", "cd"};
    auto [joined, lengths] = residual::join_parts(parts, "|");
    CHECK_FALSE(residual::split_parts(joined + "x", "|", lengths));
    CHECK_FALSE(residual::split_parts("ab-cd", "|", lengths));
    CHECK_FALSE(residual::split_parts("a", "|", lengths));
    // A length that ends inside a UTF-8 sequence.
    const std::vector<Value> inside{Value(std::int64_t{1})};
    CHECK_FALSE(residual::split_parts("é", "|", inside));
    const std::vector<Value> not_int{Value("2")};
    CHECK_FALSE(residual::split_parts("ab", "|", not_int));
    auto none = residual::split_parts("", "|", std::span<const Value>{});
    REQUIRE(none);
    CHECK(none->empty());
}

TEST_CASE("kit: take moves unkept fields and restore reads them back") {
    json::Value v = json::parse(R"({"type":"user","text":"hi","uuid":"u1","extra":{"a":1}})").value();
    auto& obj = *v.as_object();
    const std::string_view keep[] = {"type", "text"};
    auto rest = residual::take(obj, keep);
    CHECK(json::dump(v) == R"({"type":"user","text":"hi"})");
    CHECK(json::dump(json::Value(rest)) == R"({"uuid":"u1","extra":{"a":1}})");

    loa::core::Node node{};
    node.attrs.insert_or_assign(residual::attr_name("claude-code"), Value(json::dump(json::Value(rest))));
    CHECK(json::dump(json::Value(residual::restore(node, "claude-code"))) == R"({"uuid":"u1","extra":{"a":1}})");
    CHECK(residual::restore(node, "codex").empty());
    node.attrs.insert_or_assign("x-codex", Value("[1]"));
    CHECK(residual::restore(node, "codex").empty());
}
