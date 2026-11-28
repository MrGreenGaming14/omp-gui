// Port of the `models/yaml.rs` and `models/selector.rs` tests.

#include <doctest/doctest.h>

#include <string>
#include <tuple>
#include <vector>

#include "loa/settings/models/selector.hpp"
#include "loa/settings/models/yaml.hpp"

using namespace loa::settings::models;
using namespace loa::settings::models::yaml;
using selector::ModelRef;
using selector::Selector;

namespace {

const std::string CONFIG = "# omp settings, edited by hand\n"
                           "theme:\n"
                           "  dark: titanium   # favourite\n"
                           "  light: light\n"
                           "\n"
                           "modelRoles:\n"
                           "  default: anthropic/claude-sonnet-4-5 # main model\n"
                           "  # smol: openai/gpt-4.1-mini\n"
                           "  slow: \"anthropic/claude-opus-4-5:high\"\n"
                           "\n"
                           "enabledModels:\n"
                           "  - claude-sonnet-4-5\n"
                           "  - path: ~/work\n"
                           "    models:\n"
                           "      - anthropic/claude-opus-4-5\n"
                           "\n"
                           "# trailing note\n";

YamlDocument doc(std::string_view text) {
    auto d = YamlDocument::parse(text, "config.yml");
    REQUIRE(d.has_value());
    return std::move(*d);
}

std::string replace(std::string s, std::string_view from, std::string_view to) {
    const auto at = s.find(from);
    REQUIRE(at != std::string::npos);
    s.replace(at, from.size(), to);
    return s;
}

bool contains(const std::string& s, std::string_view part) { return s.find(part) != std::string::npos; }

} // namespace

TEST_CASE("setting an existing scalar changes only that line and keeps its comment") {
    auto d = doc(CONFIG);
    REQUIRE(d.set({"modelRoles", "default"}, ystr("openai/gpt-5")));
    CHECK(d.text() == replace(CONFIG, "  default: anthropic/claude-sonnet-4-5 # main model", "  default: openai/gpt-5 # main model"));
}

TEST_CASE("adding a key to a nested mapping appends after its last entry") {
    auto d = doc(CONFIG);
    REQUIRE(d.set({"modelRoles", "plan"}, ystr("@slow")));
    CHECK(d.text() == replace(CONFIG, "  slow: \"anthropic/claude-opus-4-5:high\"\n",
                              "  slow: \"anthropic/claude-opus-4-5:high\"\n  plan: \"@slow\"\n"));
    REQUIRE(d.get({"modelRoles", "plan"}));
    CHECK(*d.get({"modelRoles", "plan"}) == ystr("@slow"));
}

TEST_CASE("adding a nested path creates parents at the end of the root") {
    auto d = doc(CONFIG);
    REQUIRE(d.set({"retry", "fallbackChains", "default"}, ystrs({"openai/gpt-5", "@smol"})));
    const std::string prefix = CONFIG.substr(0, CONFIG.size() - std::string_view("\n# trailing note\n").size());
    CHECK(d.text().starts_with(prefix));
    CHECK(contains(d.text(), "retry:\n  fallbackChains:\n    default:\n      - openai/gpt-5\n      - \"@smol\"\n"));
    CHECK(contains(d.text(), "# omp settings, edited by hand"));
    CHECK(contains(d.text(), "# smol: openai/gpt-4.1-mini"));
}

TEST_CASE("replacing a list keeps neighbouring comments and keys") {
    auto d = doc(CONFIG);
    REQUIRE(d.set({"enabledModels"}, ystrs({"gpt-5"})));
    CHECK(d.text() == "# omp settings, edited by hand\n"
                      "theme:\n"
                      "  dark: titanium   # favourite\n"
                      "  light: light\n"
                      "\n"
                      "modelRoles:\n"
                      "  default: anthropic/claude-sonnet-4-5 # main model\n"
                      "  # smol: openai/gpt-4.1-mini\n"
                      "  slow: \"anthropic/claude-opus-4-5:high\"\n"
                      "\n"
                      "enabledModels:\n"
                      "  - gpt-5\n"
                      "\n"
                      "# trailing note\n");
}

TEST_CASE("removing the last child removes the empty parent") {
    auto d = doc("a: 1\nretry:\n  fallbackChains:\n    smol: [x]\nb: 2\n");
    auto removed = d.remove({"retry", "fallbackChains", "smol"});
    REQUIRE(removed);
    CHECK(*removed);
    CHECK(d.text() == "a: 1\nb: 2\n");
    auto again = d.remove({"retry"});
    REQUIRE(again);
    CHECK_FALSE(*again);
}

TEST_CASE("removing a middle key keeps the rest") {
    auto d = doc(CONFIG);
    REQUIRE(d.remove({"modelRoles", "slow"}));
    CHECK(contains(d.text(), "modelRoles:\n  default: anthropic/claude-sonnet-4-5 # main model\n  # smol: openai/gpt-4.1-mini\n\nenabledModels:"));
}

TEST_CASE("set preserves layout") {
    struct Case {
        std::string input;
        std::vector<std::string_view> path;
        Yaml value;
        std::string expected;
    };
    const std::vector<Case> cases = {
        {"providers:\n   my-gw:\n      baseUrl: http://x\n",
         {"providers", "my-gw", "modelOverrides", "m1", "name"},
         ystr("M One"),
         "providers:\n   my-gw:\n      baseUrl: http://x\n      modelOverrides:\n         m1:\n            name: M One\n"},
        {"modelRoles:\nother: 1\n", {"modelRoles", "default"}, ystr("a/b"), "modelRoles:\n  default: a/b\nother: 1\n"},
        {"", {"modelProviderOrder"}, ystrs({"anthropic"}), "modelProviderOrder:\n  - anthropic\n"},
        {"# nothing yet", {"a"}, ystr("b"), "# nothing yet\na: b\n"},
        {"a: 1\r\nb:\r\n  c: 2\r\n", {"b", "d"}, ystr("x"), "a: 1\r\nb:\r\n  c: 2\r\n  d: x\r\n"},
        {"note: |\n  # not a comment\n  text\nkeep: 1\n", {"note"}, ystr("short"), "note: short\nkeep: 1\n"},
        {"a: 1", {"b"}, ystr("2x"), "a: 1\nb: 2x\n"},
        {"a:\n  b: 1", {"a", "b"}, Yaml(std::int64_t{2}), "a:\n  b: 2"},
    };
    for (const auto& c : cases) {
        CAPTURE(c.input);
        auto d = doc(c.input);
        auto ok = d.set(std::span<const std::string_view>(c.path), c.value);
        REQUIRE_MESSAGE(ok.has_value(), (ok ? "" : ok.error().to_string()));
        CHECK(d.text() == c.expected);
    }
}

TEST_CASE("values that yaml would misread are quoted") {
    auto d = doc("");
    const std::vector<std::pair<std::string_view, std::string>> kv = {
        {"a", "yes"}, {"b", "1.5"}, {"c", "@smol"}, {"d", "x: y"}, {"e", "!op read"}};
    for (const auto& [k, v] : kv) {
        REQUIRE(d.set({k}, ystr(v)));
    }
    for (const auto& [k, v] : kv) {
        REQUIRE(d.get({k}));
        CHECK(*d.get({k}) == ystr(v));
    }
    CHECK(contains(d.text(), "e: \"!op read\""));
}

TEST_CASE("flow root and multi document files are refused") {
    auto flow = YamlDocument::parse("{a: 1}", "x");
    REQUIRE_FALSE(flow);
    CHECK(flow.error().kind == Error::Kind::Unsupported);
    auto multi = YamlDocument::parse("a: 1\n---\nb: 2\n", "x");
    REQUIRE_FALSE(multi);
    CHECK(multi.error().kind == Error::Kind::Unsupported);
    auto broken = YamlDocument::parse("a: [1", "x");
    REQUIRE_FALSE(broken);
    CHECK(broken.error().kind == Error::Kind::Parse);
}

TEST_CASE("mappings inside sequences render and read back") {
    auto d = doc("");
    const Yaml scoped = ymap({{"path", ystr("~/work")}, {"models", ystrs({"a/b", "c"})}});
    REQUIRE(d.set({"enabledModels"}, Yaml(Array{ystr("x"), scoped})));
    CHECK(d.text() == "enabledModels:\n  - x\n  - path: ~/work\n    models:\n      - a/b\n      - c\n");
    REQUIRE(d.get({"enabledModels"}));
    REQUIRE(d.get({"enabledModels"})->as_vec());
    CHECK(d.get({"enabledModels"})->as_vec()->size() == 2);
}

TEST_CASE("quoted keys are found") {
    auto d = doc("modelOverrides:\n  \"qwen3:8b\":\n    name: Q\n  'it''s': 1\n");
    REQUIRE(d.set({"modelOverrides", "qwen3:8b", "name"}, ystr("Qwen")));
    REQUIRE(d.set({"modelOverrides", "it's"}, Yaml(std::int64_t{2})));
    CHECK(d.text() == "modelOverrides:\n  \"qwen3:8b\":\n    name: Qwen\n  'it''s': 2\n");
}

TEST_CASE("scalars resolve like yaml-rust2") {
    auto docs = load_all("a: 0x1F\nb: +7\nc: 1.5\nd: 1e3\ne: ~\nf: True\ng: Null\nh: '12'\ni: inf\nj: .inf\nk: 1_000\nl: -0o7\n");
    REQUIRE(docs);
    const Yaml& r = docs->front();
    CHECK(*r.get("a") == Yaml(std::int64_t{31}));
    CHECK(*r.get("b") == Yaml(std::int64_t{7}));
    CHECK(*r.get("c") == Yaml(Yaml::Real{"1.5"}));
    CHECK(*r.get("d") == Yaml(Yaml::Real{"1e3"}));
    CHECK(r.get("e")->is_null());
    CHECK(*r.get("f") == Yaml(true));
    CHECK(*r.get("g") == ystr("Null"));
    CHECK(*r.get("h") == ystr("12"));
    CHECK(*r.get("i") == ystr("inf"));
    CHECK(*r.get("j") == Yaml(Yaml::Real{".inf"}));
    CHECK(*r.get("k") == ystr("1_000"));
    CHECK(*r.get("l") == ystr("-0o7"));
    CHECK_FALSE(load_all("a: 1\na: 2\n"));
    auto anchors = load_all("a: &x [1, 2]\nb: *x\n");
    REQUIRE(anchors);
    CHECK(*anchors->front().get("a") == *anchors->front().get("b"));
}

TEST_CASE("to_json converts scalars, sequences and mappings") {
    auto docs = load_all("a: 1\nb: [x, true]\nc: 2.5\nd: .nan\n");
    REQUIRE(docs);
    CHECK(loa::runtime::json::dump(to_json(docs->front())) == R"({"a":1,"b":["x",true],"c":2.5,"d":null})");
}

TEST_CASE("model references split only the first slash") {
    auto split = ModelRef::split("openrouter/vendor/model");
    REQUIRE(split);
    CHECK(split->first == "openrouter");
    CHECK(split->second == "vendor/model");
    CHECK(ModelRef::parse("openrouter/vendor/model") == ModelRef{"openrouter", "vendor/model"});
    for (std::string_view invalid : {"model", "/model", "provider/"}) {
        CHECK_FALSE(ModelRef::split(invalid));
        CHECK_FALSE(ModelRef::parse(invalid));
    }
}

TEST_CASE("role and thinking suffix round trip") {
    for (std::string_view text : {"@slow", "provider/model:high", "provider/vendor/model"}) {
        auto s = Selector::parse(text);
        REQUIRE(s);
        CHECK(s->to_string() == text);
    }
}

TEST_CASE("Selector::parse accepts and rejects like Rust") {
    struct Accept {
        std::string_view text;
        Selector expected;
    };
    const std::vector<Accept> accepted = {
        {"anthropic/claude-opus-4-5:high", Selector::model("anthropic/claude-opus-4-5", "high")},
        {"  @smol  ", Selector::role("smol")},
        {"@slow:off", Selector::role("slow", "off")},
        {"qwen3:8b", Selector::model("qwen3:8b")},
        {"a:b:inherit", Selector::model("a:b", "inherit")},
        {"google/*", Selector::model("google/*")},
        {"claude-?", Selector::model("claude-?")},
        {"m:HIGH", Selector::model("m:HIGH")},
        {"\u00a0x/y\u3000", Selector::model("x/y")},
    };
    for (const auto& a : accepted) {
        CAPTURE(a.text);
        auto s = Selector::parse(a.text);
        REQUIRE(s);
        CHECK(*s == a.expected);
    }
    for (std::string_view text : {"", "   ", "a/b c", "@", "@a b", "@a.b", "a,b", ":high", "@:high", "a\tb", "a\x01", "a\u00a0b",
                                  "@\u00e9"}) {
        CAPTURE(text);
        auto s = Selector::parse(text);
        REQUIRE_FALSE(s);
        CHECK(s.error().kind == Error::Kind::Invalid);
    }
    CHECK_FALSE(Selector::model("a,b").validate());
    CHECK_FALSE(Selector::model("a", "turbo").validate());
    CHECK(Selector::parse("").error().to_string() == "invalid value for selector: empty selector");
    CHECK(Selector::parse("@a b").error().to_string() == "invalid value for role: `a b` must be letters, digits, `_` or `-`");
}
