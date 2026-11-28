#include <doctest/doctest.h>

#include "loa/harness/claude/mapper.hpp"

using namespace loa::core;
using namespace loa::harness::claude;
using loa::contracts::Answer;
using loa::contracts::SessionConfig;
using loa::contracts::SessionState;
using loa::runtime::json::parse;
using loa::runtime::json::dump;

namespace {
Json json(std::string_view source) { return parse(source).value(); }
SessionConfig config() {
    SessionConfig c;
    c.session_id="claude-session";
    c.node="node";
    c.cwd="/work";
    return c;
}
std::string status(const Dom& d, NodeId id) { return std::string(d.get(id)->str_attr(attr::STATUS).value_or("")); }
}

TEST_CASE("claude: control responses include request id and preserve success and error payloads") {
    auto success=control_response("r",json("{\"behavior\":\"allow\"}"));
    CHECK(success==json("{\"response\":{\"request_id\":\"r\",\"response\":{\"behavior\":\"allow\"},\"subtype\":\"success\"},\"type\":\"control_response\"}"));
    auto error=control_response("r",std::unexpected(std::string("denied")));
    CHECK(error==json("{\"response\":{\"error\":\"denied\",\"request_id\":\"r\",\"subtype\":\"error\"},\"type\":\"control_response\"}"));
}

TEST_CASE("claude: unknown controls fail, MCP controls route and empty questions answer immediately") {
    auto sink=std::make_shared<MemorySink>();
    auto created=Mapper::create(sink,config()); REQUIRE(created);
    auto missing=created->handle(json(R"({"type":"control_request","request_id":"r"})"));
    REQUIRE(missing); REQUIRE(*missing);
    auto* write=std::get_if<Write>(&**missing); REQUIRE(write);
    CHECK(dump(write->value).find("missing request")!=std::string::npos);
    auto unsupported=created->handle(json(R"({"type":"control_request","request_id":"u","request":{"subtype":"unknown"}})"));
    REQUIRE(unsupported); REQUIRE(*unsupported); CHECK(std::holds_alternative<Write>(**unsupported));
    auto mcp=created->handle(json(R"({"type":"control_request","request_id":"m","request":{"subtype":"mcp_message","server_name":"host","message":{"jsonrpc":"2.0"}}})"));
    REQUIRE(mcp); REQUIRE(*mcp);
    auto* routed=std::get_if<Mcp>(&**mcp); REQUIRE(routed);
    CHECK(routed->request_id=="m"); CHECK(routed->server=="host");
    auto questions=created->handle(json(R"({"type":"control_request","request_id":"q","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[]}}})"));
    REQUIRE(questions); REQUIRE(*questions);
    CHECK(std::holds_alternative<Write>(**questions));
    CHECK(created->state()==SessionState::Running);
}

TEST_CASE("claude: streamed assistant text, call, result, usage and terminal result") {
    auto sink=std::make_shared<MemorySink>();
    auto created=Mapper::create(sink,config()); REQUIRE(created);
    auto& mapper=*created;
    auto turn=mapper.open_turn("hello","human","prompt-id"); REQUIRE(turn);
    CHECK(mapper.state()==SessionState::Running);
    REQUIRE(mapper.handle(json(R"({"type":"system","subtype":"init","session_id":"native","model":"opus","permissionMode":"plan"})")));
    REQUIRE(mapper.handle(json(R"({"type":"stream_event","event":{"type":"message_start","message":{"id":"m","model":"opus"}}})")));
    REQUIRE(mapper.handle(json(R"({"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}})")));
    REQUIRE(mapper.handle(json(R"({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}})")));
    REQUIRE(mapper.handle(json(R"({"type":"assistant","uuid":"assistant-uuid","message":{"id":"m","model":"opus","content":[{"type":"text","text":"hello"},{"type":"tool_use","id":"tool-1","name":"Bash","input":{"command":"echo yes"}}]}})")));
    REQUIRE(mapper.handle(json(R"({"type":"user","uuid":"result-uuid","message":{"content":[{"type":"tool_result","tool_use_id":"tool-1","content":"yes"}]}})")));
    REQUIRE(mapper.handle(json(R"({"type":"result","subtype":"success","usage":{"input_tokens":10,"output_tokens":3},"total_cost_usd":0.1,"duration_ms":42})")));
    auto dom=sink->dom();
    CHECK(mapper.native_id()==std::optional<std::string>("native"));
    CHECK(mapper.state()==SessionState::Idle);
    CHECK(status(dom,*turn)=="done");
    auto messages=dom.children_of_kind(*turn,ElementKind::Message);
    REQUIRE(messages.size()==2);
    CHECK(messages[1]->text=="hello");
    CHECK(messages[1]->str_attr(attr::ID)=="assistant-uuid");
    CHECK(status(dom,messages[1]->id)=="done");
    auto calls=dom.children_of_kind(*turn,ElementKind::Call); REQUIRE(calls.size()==1);
    CHECK(calls[0]->str_attr(UUID_ATTR)=="assistant-uuid");
    CHECK(calls[0]->str_attr(attr::TITLE)=="echo yes");
    CHECK(status(dom,calls[0]->id)=="done");
    auto results=dom.children_of_kind(calls[0]->id,ElementKind::Result); REQUIRE(results.size()==1);
    CHECK(results[0]->text=="yes");
    CHECK(results[0]->str_attr(UUID_ATTR)=="result-uuid");
    auto usage=dom.children_of_kind(*turn,ElementKind::Usage); REQUIRE(usage.size()==1);
    CHECK(usage[0]->int_attr(attr::INPUT_TOKENS)==10);
}

TEST_CASE("claude: approval, plan edit, question answers, cancellation and interrupt draining") {
    auto sink=std::make_shared<MemorySink>();
    auto created=Mapper::create(sink,config()); REQUIRE(created); auto& mapper=*created;
    REQUIRE(mapper.open_turn("go","human","p"));
    REQUIRE(mapper.handle(json(R"({"type":"control_request","request_id":"approval","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"pwd"},"permission_suggestions":[{"type":"addRules"}]}})")));
    CHECK(mapper.state()==SessionState::Waiting);
    auto dom=sink->dom(); auto turn_id=dom.children_of_kind(dom.root()->id,ElementKind::Turn)[0]->id;
    auto decisions=dom.children_of_kind(turn_id,ElementKind::Decision); REQUIRE(decisions.size()==1);
    auto response=mapper.answer(decisions[0]->id,Answer{{"allow_session"},std::nullopt,"human"}); REQUIRE(response); REQUIRE(*response);
    auto response_text=dump(**response);
    CHECK(response_text.find("updatedPermissions")!=std::string::npos);
    CHECK(mapper.state()==SessionState::Running);
    REQUIRE(mapper.handle(json(R"({"type":"control_request","request_id":"question","request":{"subtype":"can_use_tool","tool_name":"AskUserQuestion","input":{"questions":[{"question":"One?","header":"A","options":[{"label":"x"}]},{"question":"Two?","header":"B","options":[{"label":"y"}]}]}}})")));
    dom=sink->dom(); decisions=dom.children_of_kind(turn_id,ElementKind::Decision); REQUIRE(decisions.size()==3);
    auto first=mapper.answer(decisions[1]->id,Answer{{"x"},std::nullopt,"human"}); REQUIRE(first); CHECK_FALSE(*first);
    auto second=mapper.answer(decisions[2]->id,Answer{{"y"},std::nullopt,"human"}); REQUIRE(second); REQUIRE(*second);
    CHECK(dump(**second).find("One?")!=std::string::npos);
    REQUIRE(mapper.handle(json(R"({"type":"control_request","request_id":"plan","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","input":{"plan":"old"}}})")));
    dom=sink->dom(); decisions=dom.children_of_kind(turn_id,ElementKind::Decision); REQUIRE(decisions.size()==4);
    auto plan=mapper.answer(decisions[3]->id,Answer{{"edit"},std::string("new"),"human"}); REQUIRE(plan); REQUIRE(*plan);
    CHECK(dump(**plan).find("new")!=std::string::npos);
    CHECK(sink->dom().root()->str_attr(attr::MODE)=="normal");
    REQUIRE(mapper.interrupt()); CHECK(mapper.state()==SessionState::Idle);
    auto ignored=mapper.handle(json(R"({"type":"assistant","message":{"id":"late","content":[{"type":"text","text":"late"}]}})")); REQUIRE(ignored);
    REQUIRE(mapper.handle(json(R"({"type":"result","subtype":"success"})")));
    CHECK(status(sink->dom(),turn_id)=="cancelled");
}

TEST_CASE("claude: attach cancels stale nested nodes and keeps native session id") {
    auto sink=std::make_shared<MemorySink>();
    auto created=Mapper::create(sink,config()); REQUIRE(created);
    auto turn=created->open_turn("hi","person","p"); REQUIRE(turn);
    auto call=DomWriter(*sink).insert(*turn,ElementKind::Call,Attrs{{"tool","Bash"},{"status",CallStatus::Running}}); REQUIRE(call);
    REQUIRE(DomWriter(*sink).insert(*call,ElementKind::Decision,Attrs{{"kind",DecisionKind::Approval},{"status",DecisionStatus::Waiting}}));
    auto attached=Mapper::attach(sink,sink->dom()); REQUIRE(attached);
    CHECK(attached->state()==SessionState::Idle);
    CHECK(status(sink->dom(),*turn)=="cancelled");
    CHECK(status(sink->dom(),*call)=="cancelled");
}
