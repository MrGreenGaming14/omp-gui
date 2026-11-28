#include <doctest/doctest.h>

#include <string_view>
#include <variant>

#include "loa/harness/omp/rpc/event.hpp"

namespace rpc = loa::harness::omp::rpc;

namespace {
rpc::Json json(std::string_view text) {
    auto parsed = loa::runtime::json::parse(text);
    REQUIRE(parsed.has_value());
    return std::move(*parsed);
}

#define CHECK_EVENT(Type, Text) CHECK(std::holds_alternative<rpc::Type>(rpc::event_from_json(json(Text))))
#define CHECK_UI(Type, Text, Dialog) \
    do { \
        auto frame = rpc::frame_from_json(json(Text)); \
        REQUIRE(std::holds_alternative<rpc::UiRequest>(frame)); \
        const auto& request = std::get<rpc::UiRequest>(frame); \
        CHECK(std::holds_alternative<rpc::ui::Type>(request)); \
        CHECK(rpc::ui_id(request) == "r"); \
        CHECK(rpc::is_dialog(request) == Dialog); \
    } while (false)
} // namespace

TEST_CASE("omp RPC event variants decode") {
    CHECK_EVENT(event::AgentStart, R"({"type":"agent_start"})");
    CHECK_EVENT(event::TurnStart, R"({"type":"turn_start"})");
    CHECK_EVENT(event::MessageStart, R"({"type":"message_start","message":{"role":"assistant"},"messageId":"m"})");
    CHECK_EVENT(event::MessageEnd, R"({"type":"message_end"})");
    CHECK_EVENT(event::ToolExecutionStart, R"({"type":"tool_execution_start","toolCallId":"c","toolName":"read","args":{"path":"x"},"intent":"inspect"})");
    CHECK_EVENT(event::ToolExecutionUpdate, R"({"type":"tool_execution_update","toolCallId":"c","toolName":"read","partialResult":"partial"})");
    CHECK_EVENT(event::ToolExecutionEnd, R"({"type":"tool_execution_end","toolCallId":"c","toolName":"read","result":{"ok":true},"isError":true})");
    CHECK_EVENT(event::CompactionEnd, R"({"type":"auto_compaction_end","result":{"summary":"short","tokensBefore":42},"aborted":true})");
    auto manual = rpc::event_from_json(json(R"({"type":"manual_compaction_end","skipped":true})"));
    REQUIRE(std::holds_alternative<rpc::event::CompactionEnd>(manual));
    CHECK((std::get<rpc::event::CompactionEnd>(manual) == rpc::event::CompactionEnd{std::nullopt, false, true, std::nullopt}));
    CHECK_EVENT(event::ThinkingLevelChanged, R"({"type":"thinking_level_changed","thinkingLevel":"high"})");
    auto todos = rpc::event_from_json(json(R"({"type":"todo_reminder","todos":[{"content":"a","status":"pending"},{"content":"b","status":"in_progress"},{"content":"c","status":"completed"},{"content":"d","status":"abandoned"}]})"));
    REQUIRE(std::holds_alternative<rpc::event::TodoReminder>(todos));
    CHECK(std::get<rpc::event::TodoReminder>(todos).todos.size() == 4);
    CHECK(std::get<rpc::event::TodoReminder>(todos).todos[1].status == rpc::TodoStatus::InProgress);
    CHECK_EVENT(event::TodoAutoClear, R"({"type":"todo_auto_clear"})");
    CHECK_EVENT(event::Notice, R"({"type":"notice","level":"warn","message":"hello","source":"omp"})");
    auto prompt = rpc::event_from_json(json(R"({"type":"prompt_result","id":"p","agentInvoked":true,"status":"error","error":{"message":"oops","provider":"p","model":"m","retryable":true,"code":"bad"},"sessionSettled":true})"));
    REQUIRE(std::holds_alternative<rpc::event::PromptResult>(prompt));
    const auto& result = std::get<rpc::event::PromptResult>(prompt);
    CHECK(result.status == rpc::PromptStatus::Error);
    CHECK(result.error->retryable);
    CHECK(result.session_settled);
    CHECK_EVENT(event::PromptResult, R"({"type":"prompt_result","status":"completed"})");
    CHECK_EVENT(event::PromptResult, R"({"type":"prompt_result","status":"aborted"})");
    CHECK_EVENT(event::SessionSettled, R"({"type":"session_settled"})");
    CHECK_EVENT(event::ConfigUpdate, R"({"type":"config_update","model":{"id":"m","provider":"p","name":"Model","reasoning":true,"cost":2},"thinkingLevel":"low"})");
    CHECK_EVENT(event::SessionInfoUpdate, R"({"type":"session_info_update","title":"Session","sessionId":"s"})");
    auto defaults = rpc::event_from_json(json(R"({"type":"session_info_update"})"));
    CHECK(std::get<rpc::event::SessionInfoUpdate>(defaults).session_id.empty());
}

TEST_CASE("omp RPC assistant steps decode nested and malformed shapes stay raw") {
    const auto step = [](std::string_view value) {
        auto frame = json(R"({"type":"message_update","message":{"role":"assistant"},"messageId":"m"})");
        frame.set("assistantMessageEvent", json(value));
        return rpc::event_from_json(std::move(frame));
    };
    auto start = step(R"({"type":"text_start","contentIndex":0})");
    REQUIRE(std::holds_alternative<rpc::event::MessageUpdate>(start));
    CHECK(std::get<rpc::event::MessageUpdate>(start).role == "assistant");
    CHECK(std::holds_alternative<rpc::assistant::TextStart>(std::get<rpc::event::MessageUpdate>(start).event));
    const auto check_step = [&](std::string_view value, auto sample) {
        using T = decltype(sample);
        auto event = step(value);
        REQUIRE(std::holds_alternative<rpc::event::MessageUpdate>(event));
        CHECK(std::holds_alternative<T>(std::get<rpc::event::MessageUpdate>(event).event));
    };
    check_step(R"({"type":"text_delta","contentIndex":0,"delta":"a"})", rpc::assistant::TextDelta{});
    check_step(R"({"type":"text_end","contentIndex":0,"content":"a"})", rpc::assistant::TextEnd{});
    check_step(R"({"type":"thinking_start","contentIndex":1})", rpc::assistant::ThinkingStart{});
    check_step(R"({"type":"thinking_delta","contentIndex":1,"delta":"thought"})", rpc::assistant::ThinkingDelta{});
    check_step(R"({"type":"thinking_end","contentIndex":1,"content":"thought"})", rpc::assistant::ThinkingEnd{});
    check_step(R"({"type":"toolcall_delta","contentIndex":2,"delta":"arg"})", rpc::assistant::ToolcallDelta{});
    check_step(R"({"type":"toolcall_end","contentIndex":2,"toolCall":{"id":"c"}})", rpc::assistant::ToolcallEnd{});
    check_step(R"({"type":"done","reason":"stop"})", rpc::assistant::Done{});
    check_step(R"({"type":"error","reason":"failed"})", rpc::assistant::Error{});
    check_step(R"({"type":"future_step","arbitrary":3})", rpc::assistant::Other{});
    auto tool = step(R"({"type":"toolcall_start","contentIndex":2,"block":{"id":"c"},"partial":{"role":"assistant"}})");
    REQUIRE(std::holds_alternative<rpc::event::MessageUpdate>(tool));
    const auto& block = std::get<rpc::assistant::ToolcallStart>(std::get<rpc::event::MessageUpdate>(tool).event);
    CHECK(block.block == json(R"({"id":"c"})"));
    CHECK(block.partial == json(R"({"role":"assistant"})"));
    CHECK(std::holds_alternative<rpc::event::Unknown>(step(R"({"type":"text_delta","contentIndex":"bad","delta":"x"})")));
    CHECK(std::holds_alternative<rpc::event::Unknown>(step(R"({"type":"toolcall_end","contentIndex":0})")));
    CHECK(std::holds_alternative<rpc::event::Unknown>(step(R"({"contentIndex":0})")));
    CHECK_EVENT(event::Unknown, R"({"type":"message_update","message":null,"assistantMessageEvent":{"type":"done","reason":"x"}})");
}

TEST_CASE("omp RPC subagent payloads and unknown events") {
    auto lifecycle = rpc::event_from_json(json(R"({"type":"subagent_lifecycle","payload":{"id":"s","agent":"explore","status":"started","description":"scan","sessionFile":"/tmp/s"}})"));
    REQUIRE(std::holds_alternative<rpc::SubagentLifecycle>(lifecycle));
    CHECK(std::get<rpc::SubagentLifecycle>(lifecycle).id == "s");
    CHECK(std::get<rpc::SubagentLifecycle>(lifecycle).status == rpc::SubagentStatus::Started);
    for (const auto status : {"completed", "failed", "aborted"}) {
        auto frame = json(R"({"type":"subagent_lifecycle","payload":{"id":"s","agent":"a"}})");
        auto payload = *frame.find("payload");
        payload.set("status", status);
        frame.set("payload", std::move(payload));
        CHECK(std::holds_alternative<rpc::SubagentLifecycle>(rpc::event_from_json(std::move(frame))));
    }
    auto progress = rpc::event_from_json(json(R"({"type":"subagent_progress","payload":{"index":0,"agent":"explore","task":"scan","parentToolCallId":"call","progress":{"id":"sub-19","status":"running","currentTool":"read"},"sessionFile":"/tmp/sub-19.jsonl"}})"));
    REQUIRE(std::holds_alternative<rpc::SubagentProgress>(progress));
    const auto& p = std::get<rpc::SubagentProgress>(progress);
    CHECK(*p.progress.find("id")->as_string() == "sub-19");
    CHECK(p.session_file == "/tmp/sub-19.jsonl");
    CHECK(p.parent_tool_call_id == "call");
    auto minimal = rpc::event_from_json(json(R"({"type":"subagent_progress","payload":{"agent":"a"}})"));
    REQUIRE(std::holds_alternative<rpc::SubagentProgress>(minimal));
    CHECK(std::get<rpc::SubagentProgress>(minimal).progress.is_null());
    auto unknown = json(R"({"type":"future_event","payload":{"n":1}})");
    CHECK(rpc::event_from_json(unknown) == rpc::Event{rpc::event::Unknown{unknown}});
    for (auto text : {R"({"type":"prompt_result","status":"exploded"})", R"({"type":"subagent_lifecycle","payload":{"agent":"a","status":"started"}})",
                      R"({"type":"subagent_progress","payload":{"agent":5}})", R"({"type":"todo_reminder","todos":[{"content":"x","status":"wrong"}]})",
                      R"({"type":"config_update","model":{"id":"x"}})", R"({"type":"tool_execution_end","toolCallId":"c"})",
                      R"({"type":"auto_compaction_end","result":{"tokensBefore":2}})", R"({"type":"prompt_result","status":"error","error":{}})"}) {
        CAPTURE(text);
        auto raw = json(text);
        CHECK(rpc::event_from_json(raw) == rpc::Event{rpc::event::Unknown{raw}});
    }
}

TEST_CASE("omp RPC UI request variants and dialogs") {
    CHECK_UI(Select, R"({"type":"extension_ui_request","method":"select","id":"r","title":"Choose","options":["a","b"],"timeout":5})", true);
    CHECK_UI(Confirm, R"({"type":"extension_ui_request","method":"confirm","id":"r","title":"Sure?"})", true);
    CHECK_UI(Input, R"({"type":"extension_ui_request","method":"input","id":"r","title":"Name","placeholder":"Type","secret":true})", true);
    CHECK_UI(Editor, R"({"type":"extension_ui_request","method":"editor","id":"r","title":"Edit","prefill":"draft"})", true);
    CHECK_UI(Ask, R"({"type":"extension_ui_request","method":"ask","id":"r","questions":[{"id":"q","question":"Why?","header":"Info","options":[{"label":"yes","description":"agree"},{"label":"no"}],"multi":true,"recommended":0}],"timeout":6})", true);
    CHECK_UI(Cancel, R"({"type":"extension_ui_request","method":"cancel","id":"r","targetId":"dialog"})", false);
    CHECK_UI(Notify, R"({"type":"extension_ui_request","method":"notify","id":"r","message":"hello","notifyType":"info"})", false);
    CHECK_UI(SetTitle, R"({"type":"extension_ui_request","method":"setTitle","id":"r","title":"Session"})", false);
    CHECK_UI(OpenUrl, R"({"type":"extension_ui_request","method":"open_url","id":"r","url":"https://example.test","launchUrl":"https://example.test/open","instructions":"click"})", false);
    auto ask = rpc::decode_as<rpc::UiRequest>(json(R"({"method":"ask","id":"r","questions":[{"id":"q","question":"Question"}]})"));
    REQUIRE(ask.has_value());
    const auto& question = std::get<rpc::ui::Ask>(*ask).questions.front();
    CHECK(question.options.empty());
    CHECK_FALSE(question.multi);
    CHECK_FALSE(question.recommended.has_value());
    CHECK(std::holds_alternative<rpc::Event>(rpc::frame_from_json(json(R"({"type":"agent_start"})"))));
}

TEST_CASE("omp RPC malformed and unsupported UI request remains unknown") {
    for (auto text : {R"({"type":"extension_ui_request","method":"widget","id":"r"})",
                      R"({"type":"extension_ui_request","method":"select","id":"r","title":"x","options":[1]})",
                      R"({"type":"extension_ui_request","method":"ask","id":"r","questions":[{"id":"q","question":"Q","options":[{}]}]})",
                      R"({"type":"extension_ui_request","method":"ask","id":"r","questions":[{"id":"q","question":"Q","recommended":-1}]})",
                      R"({"type":"extension_ui_request","method":"cancel","id":"r"})",
                      R"({"type":"extension_ui_request","method":"open_url","id":"r"})",
                      R"({"type":"extension_ui_request","method":"confirm","id":"r","title":"t","timeout":-2})",
                      R"({"type":"extension_ui_request","method":"input","id":"r","title":"t","secret":"yes"})"}) {
        CAPTURE(text);
        auto raw = json(text);
        auto frame = rpc::frame_from_json(raw);
        REQUIRE(std::holds_alternative<rpc::Event>(frame));
        CHECK(std::get<rpc::Event>(frame) == rpc::Event{rpc::event::Unknown{raw}});
        CHECK_FALSE(rpc::decode_as<rpc::UiRequest>(raw).has_value());
    }
}

TEST_CASE("omp RPC host tool call defaults and malformed shape") {
    auto call = rpc::decode_as<rpc::HostToolCall>(json(R"({"id":"r","toolCallId":"c","toolName":"read","arguments":{"path":"x"}})"));
    REQUIRE(call.has_value());
    CHECK(call->arguments == json(R"({"path":"x"})"));
    auto bare = rpc::decode_as<rpc::HostToolCall>(json(R"({"id":"r","toolCallId":"c","toolName":"read"})"));
    REQUIRE(bare.has_value());
    CHECK(bare->arguments.is_null());
    CHECK_FALSE(rpc::decode_as<rpc::HostToolCall>(json(R"({"id":"r","toolCallId":9,"toolName":"read"})")).has_value());
}
