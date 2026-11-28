#include <doctest/doctest.h>

#include <filesystem>
#include <fstream>
#include <iterator>
#include <string>

#include "loa/core/patch.hpp"
#include "loa/p2p/wire.hpp"

using namespace loa::p2p;
using namespace loa::p2p::wire;

namespace {

Bytes slurp(const std::string& rel) {
    std::ifstream in(std::filesystem::path(LOA_GOLDEN_DIR) / "p2p" / rel, std::ios::binary);
    REQUIRE(in.good());
    return {std::istreambuf_iterator<char>(in), {}};
}

// The framed bytes `send` writes for `message`.
template <class M>
Bytes framed(const M& message) {
    auto [a, b] = duplex(4 * MAX_FRAME_SIZE);
    REQUIRE(send(*a.send, message).has_value());
    REQUIRE(a.send->shutdown().has_value());
    Bytes out;
    std::uint8_t buf[4096];
    for (;;) {
        auto n = b.recv->read(buf);
        REQUIRE(n.has_value());
        if (*n == 0) break;
        out.insert(out.end(), buf, buf + *n);
    }
    return out;
}

// Decodes a golden frame as `M`, checks it re-encodes identically, returns it.
template <class M>
M golden(const std::string& rel) {
    const auto bytes = slurp(rel);
    REQUIRE(bytes.size() > 4);
    auto [a, b] = duplex(4 * MAX_FRAME_SIZE);
    REQUIRE(a.send->write_all(bytes).has_value());
    REQUIRE(a.send->shutdown().has_value());
    auto message = recv<M>(*b.recv);
    REQUIRE(message.has_value());
    REQUIRE(message->has_value());
    CHECK_MESSAGE(framed(**message) == bytes, rel);
    auto eof = recv<M>(*b.recv);
    REQUIRE(eof.has_value());
    CHECK_FALSE(eof->has_value());
    return **message;
}

Bytes fill(std::size_t n, std::uint8_t v) { return Bytes(n, v); }

} // namespace

TEST_CASE("frames round trip and bounds are enforced") {
    auto [a, b] = duplex(64 * 1024);
    const ControlFrame delivery{Control(GrantDelivery{{1, 2}, fill(32, 9)})};
    REQUIRE(send(*a.send, delivery).has_value());
    auto got = recv<ControlFrame>(*b.recv);
    REQUIRE(got.has_value());
    CHECK(*got == std::optional(delivery));

    REQUIRE(ack<Frame>(*a.send, "no").has_value());
    auto refused = expect<Frame>(*b.recv);
    REQUIRE_FALSE(refused.has_value());
    CHECK(refused.error().kind() == NodeError::Kind::refused);
    CHECK(refused.error().message() == "no");

    CHECK_FALSE(send(*a.send, GrantDelivery{fill(MAX_FRAME_SIZE, 0), {}}).has_value());
    CHECK_FALSE(send(*a.send, ListSessions{}).has_value()); // empty encoding

    const auto too_big = static_cast<std::uint32_t>(MAX_FRAME_SIZE + 1);
    const std::uint8_t header[4] = {static_cast<std::uint8_t>(too_big >> 24), static_cast<std::uint8_t>(too_big >> 16),
                                    static_cast<std::uint8_t>(too_big >> 8), static_cast<std::uint8_t>(too_big)};
    REQUIRE(a.send->write_all(header).has_value());
    auto bad = recv<Frame>(*b.recv);
    REQUIRE_FALSE(bad.has_value());
    CHECK(bad.error().kind() == NodeError::Kind::protocol);
}

TEST_CASE("a zero-length frame is refused on read") {
    auto [a, b] = duplex(64);
    const std::uint8_t zero[4] = {0, 0, 0, 0};
    REQUIRE(a.send->write_all(zero).has_value());
    auto bad = recv<Frame>(*b.recv);
    REQUIRE_FALSE(bad.has_value());
    CHECK(bad.error().kind() == NodeError::Kind::protocol);
}

TEST_CASE("envelopes: empty frames and folder refusals") {
    CHECK(into_ack(Frame{}).error().kind() == NodeError::Kind::protocol);
    CHECK(into_ack(ControlFrame{Control(Ack{})}).has_value());
    auto folder = make_ack(std::type_identity<FolderReply>{}, "denied");
    CHECK((folder.done && !folder.ready && folder.error == "denied"));
    CHECK(into_ack(folder).error().kind() == NodeError::Kind::refused);
    auto ok = make_ack(std::type_identity<FolderReply>{}, std::nullopt);
    CHECK((ok.ready && !ok.done && into_ack(ok).has_value()));
}

TEST_CASE("golden control frames decode, match and re-encode byte for byte") {
    const Bytes token{1, 2, 3, 4};
    const Bytes id = fill(16, 0xab);
    const std::pair<const char*, ControlFrame> expected[] = {
        {"control-hello.bin",
         {Control(Hello{"0.1.0", {{"omp/control", 1}, {"omp/session", 1}, {"omp/folder", 1}}, "golden"})}},
        {"control-presence.bin", {Control(Presence{true, 1'800'000'000'000})}},
        {"control-grant-delivery.bin", {Control(GrantDelivery{token, fill(32, 9)})}},
        {"control-grant-renewal.bin", {Control(GrantRenewal{id})}},
        {"control-grant-renewal-result.bin", {Control(GrantRenewalResult{token, ""})}},
        {"control-grant-renewal-refused.bin", {Control(GrantRenewalResult{std::nullopt, "revoked"})}},
        {"control-grant-revocation.bin", {Control(GrantRevocation{id})}},
        {"control-ack.bin", {Control(Ack{})}},
        {"control-ack-refused.bin", {Control(Ack{"no"})}},
    };
    for (const auto& [file, frame] : expected) {
        CAPTURE(file);
        CHECK(golden<ControlFrame>(file) == frame);
        CHECK(framed(frame) == slurp(file));
    }
}

TEST_CASE("golden session frames decode, match and re-encode byte for byte") {
    const Bytes token{1, 2, 3, 4};
    Bytes nonce;
    for (std::uint8_t i = 0; i < 32; ++i) nonce.push_back(i);
    const std::string draft = R"({"op":"append"})";
    const std::pair<const char*, Frame> expected[] = {
        {"frame-subscribe.bin", {Body(Subscribe{"golden", 7})}},
        {"frame-drive.bin", {Body(Drive{"golden", 2, Bytes(draft.begin(), draft.end()), -5})}},
        {"frame-challenge.bin", {Body(Challenge{nonce})}},
        {"frame-present.bin", {Body(Present{{PresentedGrant{token, fill(64, 0x5a)}}})}},
        {"frame-list-sessions.bin", {Body(ListSessions{})}},
        {"frame-session-list.bin",
         {Body(SessionList{{
             SessionEntry{"golden", "Title ✓", "omp", 1'700'000'000'000, {"view", "prompt"}, false, 2},
             SessionEntry{"part", "", "omp", 0, {"view"}, true, 0},
         }})}},
        {"frame-drive-result-conflict.bin", {Body(DriveResult{std::nullopt, 9, "conflict"})}},
        {"frame-prompt.bin", {Body(Prompt{"golden", "hello\nworld"})}},
        {"frame-ack.bin", {Body(Ack{})}},
        {"frame-ack-refused.bin", {Body(Ack{"no"})}},
        {"frame-answer.bin", {Body(Answer{"golden", 3, {"yes", "a"}, "because"})}},
    };
    for (const auto& [file, frame] : expected) {
        CAPTURE(file);
        CHECK(golden<Frame>(file) == frame);
        CHECK(framed(frame) == slurp(file));
    }
}

TEST_CASE("golden frames with patches carry decodable patches") {
    auto update = golden<Frame>("frame-update.bin");
    const auto& u = std::get<WireUpdate>(*update.body);
    CHECK(u.source_seq == 2);
    REQUIRE(u.patch.has_value());
    auto second = patches(std::span(&*u.patch, 1));
    REQUIRE(second.has_value());
    CHECK(to_wire(second->front()) == *u.patch);

    auto seed = golden<Frame>("frame-seed.bin");
    const auto& s = std::get<Seed>(*seed.body);
    CHECK(s.source_seq == 2);
    CHECK(s.actions == std::vector<std::string>{"view", "drive"});
    auto decoded = patches(s.patches);
    REQUIRE(decoded.has_value());
    REQUIRE(decoded->size() == 2);
    CHECK(to_wire((*decoded)[1]) == *u.patch);
    for (std::size_t i = 0; i < 2; ++i) CHECK(to_wire((*decoded)[i]) == s.patches[i]);

    auto result = golden<Frame>("frame-drive-result.bin");
    const auto& r = std::get<DriveResult>(*result.body);
    CHECK(r.patch == u.patch);
    CHECK_FALSE(r.conflict_current.has_value());
    CHECK(r.error.empty());
}

TEST_CASE("golden folder messages decode, match and re-encode byte for byte") {
    const WireEntry entry{"a.txt", false, 12};
    CHECK(golden<FolderRequest>("folder-request-list.bin") == FolderRequest{"list", "docs", "sub", 0, {}});
    CHECK(golden<FolderRequest>("folder-request-write.bin") == FolderRequest{"write", "docs", "a.txt", 3, {'a', 'b', 'c'}});
    CHECK(golden<FolderReply>("folder-reply-entries.bin") ==
          FolderReply{"", true, {entry, WireEntry{"sub", true, 0}}, {}, true});
    CHECK(golden<FolderReply>("folder-reply-refused.bin") == make_ack(std::type_identity<FolderReply>{}, "denied"));
    CHECK(golden<WireEntry>("wire-entry.bin") == entry);
}

TEST_CASE("golden labels are the hello order") {
    const auto bytes = slurp("labels.txt");
    std::string expected;
    for (auto label : LOCAL) expected += std::string(label) + "\n";
    CHECK(std::string(bytes.begin(), bytes.end()) == expected);
}

TEST_CASE("unknown fields are skipped and wrong wire types fail") {
    Bytes bytes = encode(Ack{"x"});
    bytes.insert(bytes.end(), {0x48, 0x05}); // field 9 varint
    CHECK(decode<Ack>(bytes).value() == Ack{"x"});
    const Bytes wrong{0x08, 0x01}; // field 1 as varint
    CHECK(decode<Ack>(wrong).error().kind() == NodeError::Kind::decode);
}
