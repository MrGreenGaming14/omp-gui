#include "loa/surfaces/proto/wire.hpp"

#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>

#include "loa/runtime/proto.hpp"

using namespace loa::surfaces::proto;
namespace cs = loa::contracts::surface;

namespace {

Bytes golden(const std::string& name) {
    std::ifstream in(std::filesystem::path(LOA_GOLDEN_DIR) / "surface" / name, std::ios::binary);
    REQUIRE_MESSAGE(in, "missing golden " << name);
    return {std::istreambuf_iterator<char>(in), {}};
}

// One length delimited message from the front of `bytes`, advancing it.
StreamMessage take_delimited(std::span<const std::uint8_t>& bytes) {
    loa::runtime::proto::Reader r(bytes);
    auto body = r.len();
    REQUIRE(body);
    auto msg = decode_stream_message(*body);
    REQUIRE(msg);
    bytes = bytes.subspan(r.position());
    return *msg;
}

} // namespace

TEST_CASE("inputs round trip and unknown bodies decode as none") {
    const cs::Modifiers mods{.shift = true, .ctrl = false, .alt = true, .meta = true};
    const std::vector<cs::Input> inputs{
        cs::input::MouseMove{.x = 1.5F, .y = 2.25F},
        cs::input::MouseButton{.x = 3, .y = 4, .button = cs::MouseButton::Forward, .pressed = true, .modifiers = mods},
        cs::input::Scroll{.x = 5, .y = 6, .dx = -1, .dy = 120, .modifiers = mods},
        cs::input::Key{.key = "Enter", .code = "Enter", .pressed = false, .modifiers = mods},
        cs::input::Text{.text = "héllo"},
        cs::input::Resize{.width = 800, .height = 600},
    };
    for (std::size_t i = 0; i < inputs.size(); ++i) {
        const cs::InputEvent event{.seq = i + 1, .sent_us = 42, .input = inputs[i]};
        const auto bytes = encode_to_vec(InputMsg::from(event));
        auto decoded = decode_input_msg(bytes);
        REQUIRE(decoded);
        CHECK(decoded->into_event() == event);
    }
    // Field 15, length delimited, 2 bytes: a body this version has no tag for.
    const Bytes unknown{15 << 3 | 2, 2, 0xAA, 0xBB};
    auto msg = decode_stream_message(unknown);
    REQUIRE(msg);
    CHECK_FALSE(msg->body.has_value());
}

TEST_CASE("every golden stream message and the datagram round trip byte for byte") {
    CHECK(VERSION == 1);
    const char* names[] = {"open", "accept", "accept-document", "reject", "resize", "close", "keyframe-request", "frame",
                           "document-header", "document-chunk", "input-mouse-move", "input-mouse-button", "input-scroll",
                           "input-key", "input-text", "input-resize"};
    const std::size_t body_index[] = {0, 1, 1, 2, 5, 9, 4, 6, 7, 8, 3, 3, 3, 3, 3, 3};
    for (std::size_t i = 0; i < std::size(names); ++i) {
        CAPTURE(names[i]);
        const auto bytes = golden(std::string("stream-") + names[i] + ".bin");
        std::span<const std::uint8_t> rest(bytes);
        auto msg = take_delimited(rest);
        CHECK(rest.empty());
        REQUIRE(msg.body);
        CHECK(msg.body->index() == body_index[i]);
        Bytes again;
        encode_length_delimited(again, msg);
        CHECK(again == bytes);
    }

    const auto open = golden("stream-open.bin");
    std::span<const std::uint8_t> rest(open);
    const auto o = std::get<Open>(*take_delimited(rest).body);
    CHECK(o.uri == "x11://:0/0x1");
    CHECK(o.encodings == std::vector<std::int32_t>{4, 2, 1});

    const auto key = golden("stream-input-mouse-button.bin");
    rest = key;
    const auto input = std::get<InputMsg>(*take_delimited(rest).body);
    const auto& button = std::get<MouseButtonMsg>(*input.kind);
    CHECK(button.button == static_cast<std::int32_t>(WireButton::Forward));
    CHECK(button.modifiers == 0b1011);

    const auto dg = golden("datagram-fragment.bin");
    auto datagram = decode_datagram(dg);
    REQUIRE(datagram);
    const auto& f = std::get<Fragment>(*datagram->kind);
    CHECK(f.seq == 7);
    CHECK(f.count == 2);
    REQUIRE(f.header);
    CHECK(f.header->captured_us == 1'700'000'000'000'000ULL);
    CHECK(f.header->encoding == static_cast<std::int32_t>(WireEncoding::H264));
    CHECK(f.data == Bytes{0xde, 0xad, 0xbe, 0xef});
    CHECK(encode_to_vec(*datagram) == dg);
}

TEST_CASE("the golden unknown body follows a Close and decodes as none") {
    const auto bytes = golden("stream-unknown-body.bin");
    std::span<const std::uint8_t> rest(bytes);
    const auto first = take_delimited(rest);
    REQUIRE(first.body);
    CHECK(std::get<Close>(*first.body).reason == "bye");
    CHECK_FALSE(take_delimited(rest).body.has_value());
    CHECK(rest.empty());
}
