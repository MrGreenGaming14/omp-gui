#include <doctest/doctest.h>

#include <algorithm>
#include <cstdio>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <sstream>
#include <string>

#include "loa/bus/frame.hpp"

namespace fr = loa::bus::frame;

namespace {

fr::Bytes slurp(const std::filesystem::path& p) {
    std::ifstream in(p, std::ios::binary);
    REQUIRE(in);
    return fr::Bytes(std::istreambuf_iterator<char>(in), std::istreambuf_iterator<char>());
}

std::filesystem::path golden(const char* rel) { return std::filesystem::path(LOA_GOLDEN_DIR) / rel; }

constexpr fr::Magic journal_magic{'L', 'O', 'A', 'J', 'R', 'N', 'L', 0};

fr::Bytes record(std::span<const std::uint8_t> payload) {
    fr::Bytes buf;
    REQUIRE(fr::put(buf, [&](fr::Bytes& b) { b.insert(b.end(), payload.begin(), payload.end()); }));
    return buf;
}

}  // namespace

TEST_CASE("crc32 matches the IEEE check value") {
    const std::string s = "123456789";
    CHECK(fr::crc32({reinterpret_cast<const std::uint8_t*>(s.data()), s.size()}) == 0xCBF43926u);
    CHECK(fr::crc32({}) == 0u);
}

TEST_CASE("preamble is magic then version LE") {
    const auto p = fr::preamble(journal_magic, 0x01020304);
    CHECK(p == std::array<std::uint8_t, 12>{'L', 'O', 'A', 'J', 'R', 'N', 'L', 0, 4, 3, 2, 1});
}

TEST_CASE("put appends after existing bytes and read round-trips") {
    fr::Bytes buf{9, 9};
    const fr::Bytes payload{1, 2, 3};
    REQUIRE(fr::put(buf, [&](fr::Bytes& b) { b.insert(b.end(), payload.begin(), payload.end()); }));
    REQUIRE(buf.size() == 2 + 8 + 3);
    CHECK(fr::le_u32(buf, 2) == 3);
    CHECK(fr::le_u32(buf, 6) == fr::crc32(payload));

    const auto v = fr::read(std::span(buf).subspan(2));
    CHECK(v.frame == fr::Frame{fr::Kind::whole, 3});
    CHECK(fr::Bytes(v.payload.begin(), v.payload.end()) == payload);

    std::istringstream in(std::string(buf.begin() + 2, buf.end()));
    fr::Bytes out;
    const auto f = fr::read(in, buf.size() - 2, out);
    REQUIRE(f);
    CHECK(*f == fr::Frame{fr::Kind::whole, 3});
    CHECK(out == payload);
}

TEST_CASE("read classifies short, bad length and bad checksum") {
    const fr::Bytes payload{1, 2, 3, 4};
    auto rec = record(payload);

    CHECK(fr::read(std::span(rec).first(7)).frame.kind == fr::Kind::short_);
    CHECK(fr::read(std::span(rec).first(11)).frame.kind == fr::Kind::short_);

    fr::Bytes zero(8, 0);
    CHECK(fr::read(zero).frame == fr::Frame{fr::Kind::bad_len, 0});

    fr::Bytes huge(8, 0);
    fr::put_le_u32(huge.data(), fr::max_payload + 1);
    // Runs past the room first, as in Rust.
    CHECK(fr::read(huge).frame.kind == fr::Kind::short_);
    std::istringstream hin(std::string(huge.begin(), huge.end()));
    fr::Bytes out;
    const auto hf = fr::read(hin, std::uint64_t{1} << 40, out);
    REQUIRE(hf);
    CHECK(*hf == fr::Frame{fr::Kind::bad_len, fr::max_payload + 1});

    rec.back() ^= 0xff;
    CHECK(fr::read(rec).frame == fr::Frame{fr::Kind::bad_crc, 4});
    std::istringstream cin(std::string(rec.begin(), rec.end()));
    const auto cf = fr::read(cin, rec.size(), out);
    REQUIRE(cf);
    CHECK(*cf == fr::Frame{fr::Kind::bad_crc, 4});
}

TEST_CASE("stream read reports a premature end as io") {
    for (const auto mask : {std::ios::goodbit, std::ios::failbit | std::ios::badbit}) {
        std::istringstream in(std::string("\x04\0\0\0", 4));
        in.exceptions(mask);
        fr::Bytes out;
        const auto f = fr::read(in, 100, out);
        REQUIRE_FALSE(f);
        CHECK(f.error().code() == loa::ErrorCode::io);
    }
}

TEST_CASE("put refuses a payload over the record limit") {
    fr::Bytes buf;
    const auto r = fr::put(buf, [](fr::Bytes& b) { b.resize(b.size() + fr::max_payload + 1); });
    REQUIRE_FALSE(r);
    CHECK(r.error() == std::size_t{fr::max_payload} + 1);
}

TEST_CASE("golden journals re-frame byte-identically") {
    for (const char* rel : {"journal/f/journal.loaj", "journal/g/journal.loaj", "journal/s/journal.loaj"}) {
        CAPTURE(rel);
        const auto bytes = slurp(golden(rel));
        REQUIRE(bytes.size() >= fr::preamble_len);
        const auto pre = fr::preamble(journal_magic, 1);
        CHECK(std::equal(pre.begin(), pre.end(), bytes.begin()));

        fr::Bytes rebuilt(pre.begin(), pre.end());
        std::span<const std::uint8_t> rest = std::span(bytes).subspan(fr::preamble_len);
        std::size_t records = 0;
        while (!rest.empty()) {
            const auto v = fr::read(rest);
            REQUIRE(v.frame.kind == fr::Kind::whole);
            const auto again = record(v.payload);
            rebuilt.insert(rebuilt.end(), again.begin(), again.end());
            rest = rest.subspan(fr::header_len + v.frame.len);
            ++records;
        }
        CHECK(records >= 1);  // the header record
        CHECK(rebuilt == bytes);
    }
}

TEST_CASE("golden journal g holds the golden patches in order") {
    const auto bytes = slurp(golden("journal/g/journal.loaj"));
    std::span<const std::uint8_t> rest = std::span(bytes).subspan(fr::preamble_len);
    const auto header = fr::read(rest);
    REQUIRE(header.frame.kind == fr::Kind::whole);
    rest = rest.subspan(fr::header_len + header.frame.len);
    for (int seq = 1; seq <= 8; ++seq) {
        CAPTURE(seq);
        char name[32];
        std::snprintf(name, sizeof name, "bus/patch-%02d.pb", seq);
        const auto expected = slurp(golden(name));
        const auto v = fr::read(rest);
        REQUIRE(v.frame.kind == fr::Kind::whole);
        CHECK(fr::Bytes(v.payload.begin(), v.payload.end()) == expected);
        CHECK(record(expected) == fr::Bytes(rest.begin(), rest.begin() + fr::header_len + v.frame.len));
        rest = rest.subspan(fr::header_len + v.frame.len);
    }
    CHECK(rest.empty());
}
