#include <doctest/doctest.h>

#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iterator>

#include "loa/bus/proto.hpp"
#include "loa/runtime/proto.hpp"

namespace proto = loa::bus::proto;
namespace core = loa::core;

namespace {

proto::Bytes slurp(const std::filesystem::path& p) {
    std::ifstream in(p, std::ios::binary);
    REQUIRE(in);
    return proto::Bytes(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
}

std::filesystem::path golden(const char* rel) { return std::filesystem::path(LOA_GOLDEN_DIR) / rel; }

} // namespace

TEST_CASE("golden patches and DOM decode and re-encode byte for byte") {
    core::Dom dom;
    for (int seq = 1; seq <= 8; ++seq) {
        CAPTURE(seq);
        char name[32];
        std::snprintf(name, sizeof name, "bus/patch-%02d.pb", seq);
        const auto bytes = slurp(golden(name));
        auto patch = proto::decode_patch(bytes);
        REQUIRE(patch);
        CHECK(patch->seq == static_cast<std::uint64_t>(seq));
        CHECK(proto::encode_patch(*patch) == bytes);
        REQUIRE(dom.apply(*patch));
    }
    const auto bytes = slurp(golden("bus/dom.pb"));
    auto decoded = proto::decode_dom(bytes);
    REQUIRE(decoded);
    CHECK(*decoded == dom);
    CHECK(proto::encode_dom(dom) == bytes);
}

TEST_CASE("unknown kind and missing op are rejected") {
    namespace wire = loa::runtime::proto;
    // Patch { seq: 1, node: 1, op: Insert { kind: "blink" } }
    proto::Bytes insert;
    wire::put_tag(insert, 3, wire::WireType::len);
    wire::put_len(insert, std::string_view("blink"));
    proto::Bytes op;
    wire::put_tag(op, 1, wire::WireType::len);
    wire::put_len(op, std::span<const std::uint8_t>(insert));
    proto::Bytes head;
    wire::put_tag(head, 1, wire::WireType::varint);
    wire::put_varint(head, 1);
    wire::put_tag(head, 2, wire::WireType::varint);
    wire::put_varint(head, 1);
    proto::Bytes patch = head;
    wire::put_tag(patch, 4, wire::WireType::len);
    wire::put_len(patch, std::span<const std::uint8_t>(op));

    auto bad_kind = proto::decode_patch(patch);
    REQUIRE_FALSE(bad_kind);
    CHECK(bad_kind.error() == proto::ProtoError(proto::proto_error::UnknownKind{"blink"}));
    auto no_op = proto::decode_patch(head);
    REQUIRE_FALSE(no_op);
    CHECK(no_op.error() == proto::ProtoError(proto::proto_error::MissingOp{1}));
    CHECK(proto::to_string(no_op.error()) == "patch 1 has no op");
}

TEST_CASE("every value kind round-trips") {
    const core::Value values[] = {
        core::Value(false), core::Value(std::int64_t{-42}), core::Value(0.0), core::Value(std::string()),
        core::Value(core::Value::Bytes{0, 255}),
        core::Value(core::Value::List{core::Value(true), core::Value(core::Value::List{})})};
    for (const auto& v : values) {
        proto::Bytes b;
        proto::encode(v, b);
        auto back = proto::decode_value(b);
        REQUIRE(back);
        CHECK(*back == v);
    }
    CHECK(proto::decode_value({}).error() == proto::ProtoError(proto::proto_error::MissingValue{}));
}
