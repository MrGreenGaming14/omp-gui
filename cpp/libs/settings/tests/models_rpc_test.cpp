#include <doctest/doctest.h>

#include <variant>

#include "loa/runtime/json.hpp"
#include "loa/settings/models/files.hpp"
#include "loa/settings/models/rpc.hpp"

namespace json = loa::runtime::json;
using namespace loa::settings::models;

namespace {

json::Value parse(std::string_view text) {
    auto v = json::parse(text);
    REQUIRE(v.has_value());
    return *v;
}

} // namespace

TEST_CASE("decodes_omp_login_ui_frames") {
    auto open = rpc::login_ui_request_from_json(parse(
        R"({"type":"extension_ui_request","id":"7","method":"open_url","url":"https://auth.example/authorize?x=1","launchUrl":"http://127.0.0.1:5555/l","instructions":"Sign in"})"));
    REQUIRE(open.has_value());
    CHECK(*open == rpc::LoginUiRequest{rpc::OpenUrl{"7", "https://auth.example/authorize?x=1",
                                                    "http://127.0.0.1:5555/l", "Sign in"}});

    auto input = rpc::login_ui_request_from_json(parse(
        R"({"type":"extension_ui_request","id":"8","method":"input","title":"Paste the callback URL","timeout":600000})"));
    REQUIRE(input.has_value());
    const auto* in = std::get_if<rpc::Input>(&*input);
    REQUIRE(in != nullptr);
    CHECK(in->title == "Paste the callback URL");

    auto notify = rpc::login_ui_request_from_json(parse(
        R"({"type":"extension_ui_request","id":"9","method":"notify","message":"Waiting for browser","notifyType":"info"})"));
    REQUIRE(notify.has_value());
    const auto* n = std::get_if<rpc::Notify>(&*notify);
    REQUIRE(n != nullptr);
    CHECK(n->message == "Waiting for browser");
}

TEST_CASE("encodes_ui_responses_as_omp_expects") {
    CHECK(rpc::to_json(rpc::UiResponse{rpc::UiValue{"8", "http://cb"}}) == parse(R"({"id":"8","value":"http://cb"})"));
    CHECK(rpc::to_json(rpc::UiResponse{rpc::UiCancelled{"8", true}}) == parse(R"({"id":"8","cancelled":true})"));
}

TEST_CASE("decodes_model_info_with_null_limits_and_unknown_fields") {
    auto m = rpc::model_info_from_json(parse(
        R"({"id":"m1","name":"M One","api":"openai-completions","provider":"gw","baseUrl":"http://x","reasoning":false,"input":["text"],"cost":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0},"contextWindow":null,"maxTokens":4096,"priority":3})"));
    REQUIRE(m.has_value());
    CHECK(m->selector() == "gw/m1");
    CHECK_FALSE(m->context_window.has_value());
    CHECK(m->max_tokens == std::optional<std::uint64_t>{4096});
    const json::Value extra(m->extra);
    const json::Value* priority = extra.find("priority");
    REQUIRE(priority != nullptr);
    CHECK(priority->as_i64() == std::optional<std::int64_t>{3});
}

TEST_CASE("model info round-trips through json") {
    rpc::ModelInfo m;
    m.id = "m1";
    m.name = "M One";
    m.provider = "gw";
    m.api = "openai-completions";
    m.reasoning = true;
    m.input = {"text", "image"};
    m.cost = rpc::ModelCost{1.5, 2.0, 0.25, 0.5};
    m.max_tokens = 4096;
    m.thinking = rpc::ThinkingConfig{"effort", {"low", "high"}, "high"};
    m.extra.emplace_back("baseUrl", json::Value("http://x"));
    auto back = rpc::model_info_from_json(rpc::to_json(m));
    REQUIRE(back.has_value());
    CHECK(*back == m);
}

TEST_CASE("omp files live in the agent dir") {
    files::OmpFiles f{"/home/u/.omp/agent"};
    CHECK(f.config() == std::filesystem::path("/home/u/.omp/agent/config.yml"));
    CHECK(f.models() == std::filesystem::path("/home/u/.omp/agent/models.yml"));
}
