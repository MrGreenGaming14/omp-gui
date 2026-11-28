#include <doctest/doctest.h>

#include "loa/runtime/proto.hpp"

using namespace loa::runtime::proto;

TEST_CASE("spec example: field 1 varint 150") {
    Bytes out;
    put_tag(out, 1, WireType::varint);
    put_varint(out, 150);
    CHECK(out == Bytes{0x08, 0x96, 0x01});

    Reader r(out);
    auto tag = r.tag();
    REQUIRE(tag);
    CHECK(tag->field == 1);
    CHECK(tag->type == WireType::varint);
    CHECK(r.varint().value() == 150);
    CHECK(r.done());
}

TEST_CASE("spec example: field 2 string testing round-trips byte-identically") {
    const Bytes golden{0x12, 0x07, 0x74, 0x65, 0x73, 0x74, 0x69, 0x6e, 0x67};
    Bytes out;
    put_tag(out, 2, WireType::len);
    put_len(out, std::string_view("testing"));
    CHECK(out == golden);

    Reader r(golden);
    auto tag = r.tag().value();
    CHECK(tag.field == 2);
    auto payload = r.len().value();
    CHECK(std::string(payload.begin(), payload.end()) == "testing");
    Bytes again;
    put_tag(again, tag.field, tag.type);
    put_len(again, payload);
    CHECK(again == golden);
}

TEST_CASE("zigzag") {
    CHECK(zigzag_encode(0) == 0);
    CHECK(zigzag_encode(-1) == 1);
    CHECK(zigzag_encode(1) == 2);
    CHECK(zigzag_encode(-2) == 3);
    CHECK(zigzag_encode32(2147483647) == 4294967294u);
    CHECK(zigzag_encode32(-2147483648) == 4294967295u);
    for (std::int64_t v : std::initializer_list<std::int64_t>{0, -1, 1, -123456789, INT64_MAX, INT64_MIN})
        CHECK(zigzag_decode(zigzag_encode(v)) == v);
}

TEST_CASE("fixed and varint edge values") {
    Bytes out;
    put_fixed32(out, 0x01020304);
    put_fixed64(out, 0x0102030405060708ULL);
    put_varint(out, UINT64_MAX);
    CHECK(out.size() == 4 + 8 + 10);
    CHECK(out[0] == 0x04);
    CHECK(varint_size(UINT64_MAX) == 10);
    CHECK(varint_size(127) == 1);
    Reader r(out);
    CHECK(r.fixed32().value() == 0x01020304u);
    CHECK(r.fixed64().value() == 0x0102030405060708ULL);
    CHECK(r.varint().value() == UINT64_MAX);
    CHECK(r.done());
}

TEST_CASE("unknown fields are skipped") {
    Bytes out;
    put_tag(out, 5, WireType::fixed32);
    put_fixed32(out, 7);
    put_tag(out, 6, WireType::start_group);
    put_tag(out, 1, WireType::varint);
    put_varint(out, 3);
    put_tag(out, 6, WireType::end_group);
    put_tag(out, 7, WireType::fixed64);
    put_fixed64(out, 9);
    put_tag(out, 8, WireType::len);
    put_len(out, std::string_view("xy"));
    put_tag(out, 1, WireType::varint);
    put_varint(out, 42);

    Reader r(out);
    for (;;) {
        auto tag = r.tag().value();
        if (tag.field == 1) {
            CHECK(r.varint().value() == 42);
            break;
        }
        REQUIRE(r.skip(tag));
    }
    CHECK(r.done());
}

TEST_CASE("truncated input is an error") {
    const Bytes bad{0x12, 0x07, 0x74};
    Reader r(bad);
    REQUIRE(r.tag());
    CHECK_FALSE(r.len());
    const Bytes cut{0x96};
    CHECK_FALSE(Reader(cut).varint());
}
