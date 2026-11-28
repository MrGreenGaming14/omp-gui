#include "loa/surfaces/proto/fragment.hpp"

#include <doctest/doctest.h>

#include <algorithm>
#include <tuple>

using namespace loa::surfaces::proto;
namespace cs = loa::contracts::surface;

namespace {

cs::Frame frame(std::uint64_t seq, cs::Encoding encoding, bool keyframe, std::size_t len) {
    Bytes data(len);
    for (std::size_t i = 0; i < len; ++i) data[i] = static_cast<std::uint8_t>(i * 7 + seq);
    return {.seq = seq,
            .captured_us = seq,
            .input_seq = seq,
            .width = 64,
            .height = 32,
            .region = {.x = 0, .y = 0, .width = 64, .height = 32},
            .encoding = encoding,
            .keyframe = keyframe,
            .data = std::move(data)};
}

std::vector<Bytes> datagrams(const cs::Frame& f, std::size_t max) {
    std::vector<Bytes> out;
    REQUIRE(fragment(f, max, [&](Bytes d) -> Result<void> {
        out.push_back(std::move(d));
        return {};
    }));
    return out;
}

} // namespace

TEST_CASE("reassembly orders, drops and waits for keyframes") {
    // Reordered and duplicated fragments come back as one frame.
    const auto original = frame(1, cs::Encoding::Jpeg, true, 10'000);
    auto parts = datagrams(original, 1200);
    CHECK(parts.size() == 9);
    CHECK(std::ranges::all_of(parts, [](const Bytes& d) { return d.size() <= 1200; }));
    std::ranges::reverse(parts);
    parts.insert(parts.begin() + 4, parts[2]);
    Reassembler r;
    std::vector<cs::Frame> delivered;
    for (const auto& d : parts)
        if (auto p = r.push_datagram(d); p.frame) delivered.push_back(*p.frame);
    CHECK(delivered == std::vector{original});

    auto small = fragment(frame(1, cs::Encoding::Png, true, 10), 20, [](Bytes) -> Result<void> { return {}; });
    REQUIRE_FALSE(small);
    CHECK(small.error().kind() == ProtoError::Kind::DatagramTooSmall);
    CHECK(small.error().max() == 20);
    const auto bogus = encode_to_vec(Datagram{.kind = Fragment{.seq = 9, .index = 3, .count = 2, .header = std::nullopt, .data = {}}});
    for (const auto& junk : {bogus, Bytes{0xff, 0xff, 0xff}}) CHECK(r.push_datagram(junk) == Pushed{});

    // An incomplete frame is dropped once a newer one completes; older frames never show.
    auto late = datagrams(frame(2, cs::Encoding::Jpeg, true, 2000), 600);
    const auto missing = late[1];
    late.erase(late.begin() + 1);
    for (const auto& d : late) CHECK_FALSE(r.push_datagram(d).frame);
    const auto f3 = frame(3, cs::Encoding::Jpeg, true, 2000);
    std::vector<cs::Frame> shown;
    for (const auto& d : datagrams(f3, 600))
        if (auto p = r.push_datagram(d); p.frame) shown.push_back(*p.frame);
    CHECK(shown == std::vector{f3});
    CHECK(r.push_datagram(missing) == Pushed{});
    CHECK(r.push_frame(frame(1, cs::Encoding::Png, true, 100)) == Pushed{});
    // Region updates keep flowing after a loss and ask for a refresh.
    const auto region = frame(5, cs::Encoding::Png, false, 10);
    CHECK(r.push_frame(region) == Pushed{.frame = region, .request_keyframe = true});

    // Video: (seq, keyframe) -> (shown, keyframe requested).
    Reassembler video;
    const std::tuple<std::uint64_t, bool, bool, bool> steps[] = {
        {1, false, false, false}, {2, true, true, false}, {3, false, true, false}, {5, false, false, true},
        {6, false, false, false}, {7, true, true, false}, {8, false, true, false}};
    for (const auto& [seq, keyframe, want_shown, asks] : steps) {
        CAPTURE(seq);
        const auto pushed = video.push_frame(frame(seq, cs::Encoding::H264, keyframe, 50));
        CHECK(pushed.frame.has_value() == want_shown);
        CHECK(pushed.request_keyframe == asks);
    }
}
