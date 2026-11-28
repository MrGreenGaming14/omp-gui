#include <doctest/doctest.h>

#include <algorithm>
#include <fstream>
#include <sstream>

#include "loa/sandbox/channel.hpp"
#include "loa/sandbox/proto.hpp"

namespace proto = loa::sandbox::proto;
using loa::sandbox::Result;

namespace {

std::string golden(const std::string& name) {
    std::ifstream in(std::string(LOA_GOLDEN_DIR) + "/sandbox/" + name + ".bin", std::ios::binary);
    REQUIRE(in);
    std::ostringstream text;
    text << in.rdbuf();
    return text.str();
}

class MemoryReader final : public loa::sandbox::ByteReader {
  public:
    explicit MemoryReader(std::string data, std::size_t chunk = 7) : data_(std::move(data)), chunk_(chunk) {}
    Result<std::size_t> read(std::span<char> buf) override {
        const auto n = std::min({buf.size(), data_.size() - pos_, chunk_});
        std::copy_n(data_.data() + pos_, n, buf.data());
        pos_ += n;
        return n;
    }

  private:
    std::string data_;
    std::size_t pos_ = 0;
    std::size_t chunk_;
};

class MemoryWriter final : public loa::sandbox::ByteWriter {
  public:
    Result<> write_all(std::string_view bytes) override {
        data += bytes;
        return {};
    }
    Result<> shutdown() override { return {}; }
    std::string data;
};

std::string body(const std::string& frame) {
    REQUIRE(frame.size() >= 4);
    return frame.substr(4);
}

} // namespace

TEST_CASE("golden requests from Rust proto::frame decode and re-encode byte-identically") {
    proto::ExecRequest full;
    full.argv = {"tar", "-cf", "-"};
    full.cwd = "sub/dir";
    full.env = {{"A", "1"}, {"LONG", std::string(200, 'x')}};
    std::string all_bytes;
    for (int i = 0; i < 256; ++i) all_bytes.push_back(static_cast<char>(i));
    full.stdin_data = all_bytes;
    full.timeout_ms = 30'000'000'000ULL;
    const std::pair<std::string, proto::Request> cases[] = {
        {"request_exec_shell", proto::request::Exec{1, proto::ExecRequest::shell("printf \"$1\"", {"out"})}},
        {"request_exec_full", proto::request::Exec{300, full}},
        {"request_exec_default", proto::request::Exec{0, {}}},
        {"request_shutdown", proto::request::Shutdown{}},
    };
    for (const auto& [name, request] : cases) {
        CAPTURE(name);
        const auto bytes = golden(name);
        CHECK(proto::frame(request) == bytes);
        auto decoded = proto::decode_request(body(bytes));
        REQUIRE(decoded);
        CHECK(*decoded == request);
        CHECK(proto::frame(*decoded) == bytes);
    }
}

TEST_CASE("golden responses from Rust proto::frame decode and re-encode byte-identically") {
    const std::pair<std::string, proto::Response> cases[] = {
        {"response_ready", proto::response::Ready{proto::VERSION}},
        {"response_stdout", proto::response::Stdout{7, "out\n"}},
        {"response_stderr", proto::response::Stderr{128, std::string(300, '\0')}},
        {"response_exit_code", proto::response::Exit{2, {-7, std::nullopt, false}}},
        {"response_exit_signal", proto::response::Exit{70000, {std::nullopt, 9, true}}},
        {"response_error", proto::response::Error{0xffffffffu, "spawn: No such file or directory (os error 2)"}},
    };
    for (const auto& [name, response] : cases) {
        CAPTURE(name);
        const auto bytes = golden(name);
        CHECK(proto::frame(response) == bytes);
        auto decoded = proto::decode_response(body(bytes));
        REQUIRE(decoded);
        CHECK(*decoded == response);
        CHECK(proto::frame(*decoded) == bytes);
    }
}

TEST_CASE("write_hello writes the magic and the Ready frame the Rust stub writes") {
    MemoryWriter out;
    REQUIRE(proto::write_hello(out));
    CHECK(out.data == golden("hello"));
    CHECK(out.data.starts_with(proto::MAGIC));
    CHECK(proto::MAGIC.size() == 12);
}

TEST_CASE("the host finds the magic after boot messages and reads frames until a clean end") {
    using namespace std::string_literals;
    std::string stream = "Booting...\n\0LOA-STUB\0\0\0LOA-STUB/1\n"s;
    stream.append("\0LOA-STUB/1\n", 12);
    stream += golden("hello");
    stream += proto::frame(proto::Response{proto::response::Stdout{1, "x"}});
    MemoryReader reader(stream);
    auto found = proto::skip_to_magic(reader);
    REQUIRE(found);
    CHECK(*found);
    proto::Bytes buf;
    auto frame = proto::read_frame(reader, buf);
    REQUIRE(frame);
    REQUIRE(*frame);
    CHECK(proto::decode_response(buf) == Result<proto::Response>(proto::response::Ready{2}));
    REQUIRE(proto::read_frame(reader, buf).value());
    CHECK(proto::decode_response(buf) == Result<proto::Response>(proto::response::Stdout{1, "x"}));
    auto end = proto::read_frame(reader, buf);
    REQUIRE(end);
    CHECK_FALSE(*end);

    MemoryReader none("no stub here");
    CHECK(proto::skip_to_magic(none) == Result<bool>(false));
}

TEST_CASE("frames over the limit, truncated bodies and bad postcard are refused") {
    proto::Bytes buf;
    std::string huge("\x01\x00\x00\x04", 4);  // 64 MiB + 1
    MemoryReader over(huge);
    auto r = proto::read_frame(over, buf);
    REQUIRE_FALSE(r);
    CHECK(r.error().message() == "frame of 67108865 bytes is over the limit");

    MemoryReader cut(std::string("\x05\x00\x00\x00\x00", 5));
    CHECK_FALSE(proto::read_frame(cut, buf));

    CHECK_FALSE(proto::decode_request(std::string_view("\x02", 1)));
    CHECK_FALSE(proto::decode_response(std::string_view("\x03\x01\x02", 3)));
    CHECK_FALSE(proto::decode_response(std::string_view("\x04\x01\x02\xff\xfe", 5)));  // invalid UTF-8
    CHECK(proto::response_id(proto::response::Ready{2}) == std::nullopt);
    CHECK(proto::response_id(proto::response::Exit{9, {}}) == 9u);
}
