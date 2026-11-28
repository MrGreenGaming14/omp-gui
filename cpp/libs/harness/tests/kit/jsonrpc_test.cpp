#include <doctest/doctest.h>

#include <sstream>

#include "loa/harness/kit/jsonrpc.hpp"
#include "support.hpp"

using namespace loa::harness::kit;
namespace json = loa::runtime::json;
using loa::harness::test::golden;
using loa::harness::test::read_file;

namespace {

std::vector<RpcError> errors() {
    return {
        RpcError::NotFound{"omp"},
        RpcError::Io{loa::Error(loa::ErrorCode::io, "broken pipe")},
        RpcError::Exited{ExitStatus{3 << 8}, "boom"},
        RpcError::Exited{ExitStatus{9}, ""},
        RpcError::Exited{std::nullopt, ""},
        RpcError::Version{{3, 4}, 1, 2},
        RpcError::Version{{}, 1, 1},
        RpcError::Protocol{"bad frame"},
        RpcError::Command{"fork", "busy", "session_busy"},
        RpcError::StartTimeout{std::chrono::seconds(20)},
        RpcError::StartTimeout{std::chrono::milliseconds(1500)},
        RpcError::StartTimeout{std::chrono::milliseconds(250)},
        RpcError::Closed{},
    };
}

Json parse(std::string_view text) { return json::parse(text).value(); }

} // namespace

TEST_CASE("kit: RpcError display texts match the Rust golden") {
    std::string text;
    for (const auto& e : errors()) text += e.message() + "\n";
    CHECK(text == read_file(golden("harness/rpc_errors.txt")));
    CHECK(RpcError(RpcError::Command{"fork", "busy", "session_busy"}).code() == "session_busy");
    CHECK_FALSE(RpcError(RpcError::Closed{}).code());
}

TEST_CASE("kit: JsonRpc2 frames are byte-identical to the Rust golden") {
    std::string lines;
    for (bool header : {true, false}) {
        const JsonRpc2 f(header);
        auto push = [&](const Json& v) { lines += json::dump(v) + "\n"; };
        push(f.request(Json(1), "initialize", parse(R"({"protocolVersion":1,"b":[true,null,2.5,"é\n"]})")));
        push(f.request(Json("s-1"), "session/prompt", Json()));
        push(f.notification("initialized", Json()));
        push(f.notification("session/cancel", parse(R"({"sessionId":"x"})")));
        push(f.response(Json(7), "fs/read", parse(R"({"content":"hi"})")));
        push(f.response(Json("r"), "fs/read", std::unexpected(RpcError(RpcError::Command{"request", "nope", "-32601"}))));
        push(f.response(Json(8), "fs/read", std::unexpected(RpcError(RpcError::Command{"request", "str code", "\"x\""}))));
        for (auto& e : errors()) push(f.response(Json(9), "m", std::unexpected(e)));
    }
    CHECK(lines == read_file(golden("harness/jsonrpc.jsonl")));
}

TEST_CASE("kit: JsonRpc2 classifies requests, notifications and responses") {
    const JsonRpc2 f;
    auto req = f.classify(parse(R"({"jsonrpc":"2.0","id":4,"method":"fs/read","params":{"p":1}})"));
    REQUIRE(req);
    auto* r = std::get_if<incoming::Request>(&*req);
    REQUIRE(r);
    CHECK(r->id == Json(4));
    CHECK(r->method == "fs/read");
    CHECK(json::dump(r->params) == R"({"p":1})");

    auto note = f.classify(parse(R"({"method":"update","id":null})"));
    REQUIRE(note);
    auto* n = std::get_if<incoming::Notification>(&*note);
    REQUIRE(n);
    CHECK(n->params.is_null());

    auto ok = f.classify(parse(R"({"id":"a","result":{"x":1}})"));
    auto* okr = std::get_if<incoming::Response>(&*ok);
    REQUIRE(okr);
    CHECK(okr->id == "a");
    CHECK(json::dump(okr->result.value()) == R"({"x":1})");

    auto empty = f.classify(parse(R"({"id":2})"));
    CHECK(std::get<incoming::Response>(*empty).result.value().is_null());

    auto err = f.classify(parse(R"({"id":3,"error":{"code":-32601,"message":"no such method"}})"));
    auto& e = std::get<incoming::Response>(*err);
    CHECK(e.id == "3");
    REQUIRE_FALSE(e.result);
    CHECK(e.result.error().code() == "-32601");
    CHECK(e.result.error().message() == "the harness rejected `request`: no such method");

    auto str_code = f.classify(parse(R"({"id":3,"error":{"code":"E"}})"));
    auto& s = std::get<incoming::Response>(*str_code).result.error();
    CHECK(s.code() == "\"E\"");
    CHECK(s.get<RpcError::Command>()->message == "error");

    CHECK_FALSE(f.classify(parse(R"({"foo":1})")));
}

TEST_CASE("kit: id_key renders string ids as is and others as JSON") {
    CHECK(id_key(Json("abc")) == "abc");
    CHECK(id_key(Json(12)) == "12");
    CHECK(id_key(parse(R"({"a":1})")) == R"({"a":1})");
    CHECK(json::dump(decode(Json())) == "{}");
    CHECK(json::dump(decode(Json(1))) == "1");
}

TEST_CASE("kit: RpcError converts to AdapterError") {
    using loa::contracts::AdapterError;
    CHECK(RpcError(RpcError::NotFound{"x"}).to_adapter_error().get<AdapterError::NotInstalled>()->executable == "x");
    CHECK(RpcError(RpcError::Io{loa::Error(loa::ErrorCode::io, "e")}).to_adapter_error().get<AdapterError::Io>());
    CHECK(RpcError(RpcError::Closed{}).to_adapter_error().get<AdapterError::Closed>());
    auto protocol = RpcError(RpcError::Protocol{"bad"}).to_adapter_error();
    REQUIRE(protocol.get<AdapterError::Protocol>());
    CHECK(protocol.get<AdapterError::Protocol>()->detail == "RPC protocol error: bad");
}
