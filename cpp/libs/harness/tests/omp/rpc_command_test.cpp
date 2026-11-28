#include <doctest/doctest.h>

#include "loa/harness/omp/rpc/command.hpp"
#include "loa/harness/omp/rpc/inbound.hpp"

using namespace loa::harness::omp::rpc;
namespace json = loa::runtime::json;

TEST_CASE("omp rpc: commands use snake_case tags, camelCase fields and omitted optionals") {
    const std::vector<Command> commands{
        command::Prompt{"hello", {}, StreamingBehavior::FollowUp}, command::Steer{"next"}, command::Abort{},
        command::NewSession{}, command::AbortAndRestoreQueue{}, command::NegotiateProtocol{2},
        command::GetState{}, command::GetEntries{}, command::GetSubagents{},
        command::GetSubagentMessages{"sub", 5}, command::SteerSubagent{"sub", "hi"},
        command::SetAskDialog{true}, command::SetHostTools{},
        command::SetSubagentSubscription{SubagentLevel::Progress}, command::SetEventFilter{},
        command::SetModel{"provider", "model"}, command::GetAvailableModels{},
        command::SetThinkingLevel{"high"}, command::Compact{}, command::Fork{},
        command::GetLoginProviders{}, command::Login{"p"}, command::GetLogoutAccounts{"p"}, command::Logout{"p", 42}};
    const std::vector<std::string_view> tags{
        "prompt", "steer", "abort", "new_session", "abort_and_restore_queue", "negotiate_protocol",
        "get_state", "get_entries", "get_subagents", "get_subagent_messages", "steer_subagent",
        "set_ask_dialog", "set_host_tools", "set_subagent_subscription", "set_event_filter",
        "set_model", "get_available_models", "set_thinking_level", "compact", "fork",
        "get_login_providers", "login", "get_logout_accounts", "logout"};
    REQUIRE(commands.size() == tags.size());
    for (std::size_t i = 0; i < tags.size(); ++i) {
        INFO("command ", tags[i]);
        const auto frame = to_json(commands[i]);
        REQUIRE(frame.find("type"));
        CHECK(*frame.find("type")->as_string() == tags[i]);
    }
    CHECK(json::dump(to_json(commands[0])) == R"({"type":"prompt","message":"hello","streamingBehavior":"followUp"})");
    CHECK(json::dump(to_json(commands[14])) == R"({"type":"set_event_filter","events":null})");
    CHECK(json::dump(to_json(commands[19])) == R"({"type":"fork"})");
    CHECK(json::dump(to_json(commands[23])) == R"({"type":"logout","providerId":"p","credentialId":42})");
    CHECK(json::dump(to_json(HostToolDefinition{"x", {}, "Search", json::Object{{"type", "object"}}, {}, {}})) ==
          R"({"name":"x","description":"Search","parameters":{"type":"object"}})");
}

TEST_CASE("omp rpc: ready chooses highest offered version and result structs decode") {
    CHECK(Ready{}.offered_versions() == std::vector<std::uint32_t>{1});
    auto ready = decode_as<Ready>(json::parse(R"({"protocolVersion":1,"supportedProtocolVersions":[1,2],"supportsBashExcludeFromContext":true})").value());
    REQUIRE(ready);
    CHECK(ready->offered_versions() == std::vector<std::uint32_t>{1, 2});
    CHECK(ready->supports_bash_exclude_from_context);
    auto state = decode_as<SessionState>(json::parse(R"({"sessionId":"s-1","thinkingLevel":"high","extra":9})").value());
    REQUIRE(state);
    CHECK(state->session_id == "s-1");
    CHECK(state->thinking_level == "high");
    CHECK(state->extra.size() == 1);
    CHECK(decode_as<PromptAck>(json::Object{})->agent_invoked == std::nullopt);
    CHECK_FALSE(decode_as<CancellationResult>(json::Object{}));
}

TEST_CASE("omp rpc: UI responses and host results match wire fields") {
    CHECK(json::dump(to_json(UiResponse(ui_response::Value{"x", "yes"}))) ==
          R"({"type":"extension_ui_response","id":"x","value":"yes"})");
    CHECK(json::dump(to_json(UiResponse(ui_response::Confirmed{"x", true}))) ==
          R"({"type":"extension_ui_response","id":"x","confirmed":true})");
    CHECK(json::dump(to_json(UiResponse(ui_response::Cancelled{"x", true}))) ==
          R"({"type":"extension_ui_response","id":"x","cancelled":true,"timedOut":true})");
    CHECK(json::dump(to_json(UiResponse(ui_response::Answers{"x", {AskAnswer{"q", {"a"}, "other"}}}))) ==
          R"({"type":"extension_ui_response","id":"x","answers":[{"id":"q","selectedOptions":["a"],"customInput":"other"}]})");
    auto result = HostToolOutput::error("oops").result_json("h1");
    CHECK(json::dump(result) ==
          R"({"type":"host_tool_result","id":"h1","result":{"content":[{"type":"text","text":"oops"}]},"isError":true})");
    auto image = HostToolOutput::blocks({ImageContent{"Zg==", "image/png"}}).result_json("h2");
    CHECK(json::dump(image) ==
          R"({"type":"host_tool_result","id":"h2","result":{"content":[{"type":"image","data":"Zg==","mimeType":"image/png"}]}})");
}
