#include <doctest/doctest.h>

#include "loa/harness/opencode/event.hpp"
#include "loa/runtime/json.hpp"

namespace oc = loa::harness::opencode;

TEST_CASE("opencode sse decoder dispatches data on blank lines across chunks") {
    oc::SseDecoder decoder;
    std::vector<std::string> out;
    decoder.feed("data: {\"a\"", out);
    CHECK(out.empty());
    decoder.feed(":1}\r\n\r\nevent: x\ndata:b\ndata: c\n", out);
    REQUIRE(out.size() == 1);
    CHECK(out[0] == R"({"a":1})");
    decoder.feed("\n", out);
    REQUIRE(out.size() == 2);
    CHECK(out[1] == "b\nc");
}

TEST_CASE("opencode events expose their session id and keep unknown properties") {
    auto known = oc::parse_event(loa::runtime::json::parse(
        R"({"type":"message.part.delta","properties":{"sessionID":"ses_1","messageID":"m","partID":"p","field":"text","delta":"hi"}})").value());
    REQUIRE(known);
    REQUIRE(known->known);
    CHECK(known->known->kind == oc::Known::Kind::PartDelta);
    CHECK(known->session_id() == "ses_1");
    CHECK(known->known->at.part == "p");
    auto other = oc::parse_event(loa::runtime::json::parse(R"({"type":"lsp.updated","properties":{"sessionID":"ses_2","x":1}})").value());
    REQUIRE(other);
    CHECK_FALSE(other->known);
    CHECK(other->session_id() == "ses_2");
    CHECK(oc::parse_todos(loa::runtime::json::parse(R"([{"content":"a","status":"pending"}])").value())->at(0).priority.empty());
    CHECK_FALSE(oc::parse_todos(loa::runtime::json::parse(R"([{"content":"a"}])").value()));
}
