#include <doctest/doctest.h>

#include "loa/core/base64.hpp"
#include "loa/harness/omp/rpc/frames.hpp"

using namespace loa::harness::omp::rpc;
namespace json = loa::runtime::json;
namespace kit = loa::harness::kit;

namespace {
std::vector<Chunk> chunks(std::string_view id, std::string_view payload, std::size_t parts) {
    std::vector<Chunk> result;
    const auto length = (payload.size() + parts - 1) / parts;
    for (std::size_t start = 0; start < payload.size(); start += length) {
        auto piece = payload.substr(start, length);
        const auto* bytes = reinterpret_cast<const std::uint8_t*>(piece.data());
        result.push_back(Chunk{std::string(id), static_cast<std::uint32_t>(result.size()), 0, payload.size(),
                               loa::core::base64::encode(std::span(bytes, piece.size()))});
    }
    for (auto& chunk : result) chunk.count = static_cast<std::uint32_t>(result.size());
    return result;
}
} // namespace

TEST_CASE("omp rpc: reassembles in order and rejects gaps interleaving and wrong lengths") {
    constexpr std::string_view payload = "0123456789abcdef";
    Reassembler r(1 << 20);
    auto parts = chunks("ok", payload, 3);
    CHECK(r.push(parts[0]).value() == std::nullopt);
    CHECK(r.push(parts[1]).value() == std::nullopt);
    CHECK(r.push(parts[2]).value() == payload);
    auto a = chunks("a", payload, 2), b = chunks("b", payload, 2);
    CHECK(r.push(a[1]).error().get<chunk_error::OutOfOrder>());
    REQUIRE(r.push(a[0]));
    CHECK(r.push(b[1]).error().get<chunk_error::OutOfOrder>());
    CHECK_FALSE(r.interrupt());
    auto lying = chunks("c", payload, 2);
    for (auto& c : lying) c.byte_length = 99;
    REQUIRE(r.push(lying[0]));
    CHECK(r.push(lying[1]).error().get<chunk_error::Length>());
    CHECK(Reassembler(4).push(chunks("d", payload, 2)[0]).error().get<chunk_error::TooLarge>());
}

TEST_CASE("omp rpc: chunks enforce canonical base64, declared lengths and interruptions") {
    Reassembler r(20);
    CHECK(r.push(Chunk{"bad", 0, 1, 1, "/w=="}).value() == std::string("\xff", 1));
    CHECK(r.push(Chunk{"bad", 0, 1, 1, "/x=="}).error().get<chunk_error::Base64>());
    CHECK(r.push(Chunk{"bad", 0, 1, 1, "Zg="}).error().get<chunk_error::Base64>());
    REQUIRE(r.push(Chunk{"a", 0, 2, 2, "YQ=="}));
    CHECK(r.interrupt()->get<chunk_error::Interrupted>());
    CHECK(r.push(Chunk{"a", 1, 2, 2, "Yg=="}).error().get<chunk_error::OutOfOrder>());
}

TEST_CASE("omp rpc: framing sends omp envelopes and classifies response errors") {
    OmpFrames frames;
    CHECK(json::dump(frames.request("c1", "prompt", json::Object{{"type", "prompt"}, {"message", "hi"}})) ==
          R"({"type":"prompt","message":"hi","id":"c1"})");
    CHECK(json::dump(frames.notification("extension_ui_response", json::Object{{"id", "x"}})) ==
          R"({"type":"extension_ui_response"})");
    auto response = frames.classify(json::parse(R"({"type":"response","id":"c5","command":"fork","success":false,"error":"busy","code":"session_busy"})").value());
    REQUIRE(response);
    const auto* failed = std::get_if<kit::incoming::Response>(&*response);
    REQUIRE(failed);
    CHECK_FALSE(failed->result);
    CHECK(failed->result.error().code() == "session_busy");
    auto uri = frames.response("u1", "host_uri_request", std::unexpected(RpcError::Closed{}));
    CHECK(*uri.find("type")->as_string() == "host_uri_result");
    CHECK(*uri.find("isError")->as_bool());
    auto tool = frames.response("t1", "host_tool_call", std::unexpected(RpcError::Closed{}));
    CHECK(*tool.find("type")->as_string() == "host_tool_result");
}

TEST_CASE("omp rpc: framing reassembles JSONL response, never nested chunks") {
    OmpFrames frames;
    constexpr std::string_view body = R"({"type":"response","id":"c1","command":"get_state","success":true,"data":{"sessionId":"big"}})";
    auto parts = chunks("k", body, 2);
    CHECK_FALSE(frames.classify(parts[0].to_json()));
    auto frame = frames.classify(parts[1].to_json());
    REQUIRE(frame);
    const auto* response = std::get_if<kit::incoming::Response>(&*frame);
    REQUIRE(response);
    REQUIRE(response->result);
    CHECK(*response->result->find("sessionId")->as_string() == "big");
    REQUIRE_FALSE(frames.classify(chunks("nested", R"({"type":"rpc_chunk"})", 1)[0].to_json()));
}
