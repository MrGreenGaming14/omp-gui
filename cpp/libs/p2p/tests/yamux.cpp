#include <doctest/doctest.h>

#include <sys/socket.h>
#include <unistd.h>

#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <iterator>
#include <mutex>
#include <thread>

#include "loa/p2p/tcp.hpp"
#include "loa/p2p/yamux.hpp"

using namespace loa::p2p;
using namespace loa::p2p::yamux;

namespace {

Bytes read_golden(const std::string& name) {
    std::ifstream in(std::string(LOA_GOLDEN_DIR) + "/p2p/yamux/" + name, std::ios::binary);
    REQUIRE(in.good());
    return Bytes(std::istreambuf_iterator<char>(in), {});
}

// Hands out `input`, then end of stream; records what is written.
class ScriptSocket final : public Socket {
  public:
    explicit ScriptSocket(Bytes input, bool hold = false) : input_(std::move(input)), hold_(hold) {}
    Result<std::size_t> read(std::span<std::uint8_t> buf) override {
        std::unique_lock lock(m_);
        if (pos_ == input_.size()) {
            cv_.wait(lock, [&] { return !hold_ || down_; });
            return std::size_t{0};
        }
        const auto n = std::min(buf.size(), input_.size() - pos_);
        std::copy_n(input_.begin() + static_cast<std::ptrdiff_t>(pos_), n, buf.begin());
        pos_ += n;
        return n;
    }
    Result<> write_all(std::span<const std::uint8_t> data) override {
        std::lock_guard lock(m_);
        written.insert(written.end(), data.begin(), data.end());
        return {};
    }
    void shutdown() override {
        std::lock_guard lock(m_);
        down_ = true;
        cv_.notify_all();
    }
    Bytes output() {
        std::lock_guard lock(m_);
        return written;
    }

  private:
    std::mutex m_;
    std::condition_variable cv_;
    Bytes input_;
    std::size_t pos_ = 0;
    bool hold_;
    bool down_ = false;
    Bytes written;
};

// One end of a socketpair.
class FdSocket final : public Socket {
  public:
    explicit FdSocket(int fd) : fd_(fd) {}
    ~FdSocket() override { ::close(fd_); }
    Result<std::size_t> read(std::span<std::uint8_t> buf) override {
        const auto n = ::read(fd_, buf.data(), buf.size());
        if (n < 0) return fail(NodeError::Kind::io, "read");
        return static_cast<std::size_t>(n);
    }
    Result<> write_all(std::span<const std::uint8_t> data) override {
        while (!data.empty()) {
            const auto n = ::write(fd_, data.data(), data.size());
            if (n <= 0) return fail(NodeError::Kind::io, "write");
            data = data.subspan(static_cast<std::size_t>(n));
        }
        return {};
    }
    void shutdown() override { ::shutdown(fd_, SHUT_RDWR); }

  private:
    int fd_;
};

std::vector<std::pair<Header, Bytes>> frames_of(const Bytes& bytes) {
    std::vector<std::pair<Header, Bytes>> out;
    std::size_t at = 0;
    while (at < bytes.size()) {
        REQUIRE(bytes.size() - at >= HEADER_SIZE);
        auto h = decode(std::span<const std::uint8_t, HEADER_SIZE>(bytes.data() + at, HEADER_SIZE));
        REQUIRE(h.has_value());
        at += HEADER_SIZE;
        const std::size_t body = h->tag == Tag::data ? h->length : 0;
        REQUIRE(bytes.size() - at >= body);
        out.emplace_back(*h, Bytes(bytes.begin() + static_cast<std::ptrdiff_t>(at),
                                   bytes.begin() + static_cast<std::ptrdiff_t>(at + body)));
        at += body;
    }
    return out;
}

std::string read_all(RecvHalf& rx) {
    std::string out;
    std::uint8_t buf[4096];
    for (;;) {
        auto n = rx.read(buf);
        REQUIRE(n.has_value());
        if (*n == 0) return out;
        out.append(reinterpret_cast<const char*>(buf), *n);
    }
}

} // namespace

TEST_CASE("yamux header codec matches the 0.14 layout") {
    const Header h = Header::window_update(0x01020304, 0x0a0b0c0d, SYN | FIN);
    const auto bytes = encode(h);
    CHECK(bytes == std::array<std::uint8_t, HEADER_SIZE>{0, 1, 0, 5, 1, 2, 3, 4, 0x0a, 0x0b, 0x0c, 0x0d});
    CHECK(decode(bytes) == h);
    auto bad_version = bytes;
    bad_version[0] = 1;
    CHECK(decode(bad_version).error().kind() == NodeError::Kind::decode);
    auto bad_tag = bytes;
    bad_tag[1] = 4;
    CHECK_FALSE(decode(bad_tag).has_value());
}

TEST_CASE("golden: Rust yamux 0.14 client bytes decode and re-encode identically") {
    const auto bytes = read_golden("rust-client.bin");
    const auto frames = frames_of(bytes);
    const std::vector<Header> expected{
        Header::ping(0, SYN),          Header::data(1, 1, SYN), Header::data(1, 4), Header::data(1, 0, FIN),
        Header::ping(0, ACK),          Header::go_away(GO_AWAY_NORMAL),
    };
    REQUIRE(frames.size() == expected.size());
    Bytes again;
    for (std::size_t i = 0; i < frames.size(); ++i) {
        CHECK(frames[i].first == expected[i]);
        const auto head = encode(frames[i].first);
        again.insert(again.end(), head.begin(), head.end());
        again.insert(again.end(), frames[i].second.begin(), frames[i].second.end());
    }
    CHECK(again == bytes);
    // The stream prefix: the first byte of the application stream is STREAM_FRAME.
    CHECK(frames[1].second == Bytes{STREAM_FRAME});
    CHECK(frames[2].second == Bytes{'p', 'i', 'n', 'g'});

    // A C++ server reads the Rust client's stream and answers its ping.
    auto socket = std::make_shared<ScriptSocket>(bytes);
    auto server = Session::start(socket, Mode::server);
    auto stream = server->accept();
    REQUIRE(stream.has_value());
    CHECK(read_all(*stream->recv) == std::string("\0ping", 5));
    CHECK(server->closed() == "connection is closed");
    // The writer flushes the queued pong after the go-away ended the session.
    for (int i = 0; i < 200 && socket->output().empty(); ++i) std::this_thread::sleep_for(std::chrono::milliseconds(5));
    const auto answer = frames_of(socket->output());
    REQUIRE_FALSE(answer.empty());
    CHECK(answer[0].first == Header::ping(0, ACK));
}

TEST_CASE("golden: the C++ client's bytes are byte-identical to cpp-client.bin") {
    auto socket = std::make_shared<ScriptSocket>(Bytes{}, true);
    auto client = Session::start(socket, Mode::client);
    {
        auto stream = client->open();
        REQUIRE(stream.has_value());
        REQUIRE(stream->send->write_all(Bytes{STREAM_FRAME}).has_value());
        REQUIRE(stream->send->write_all(Bytes{'p', 'i', 'n', 'g'}).has_value());
        REQUIRE(stream->send->shutdown().has_value());
    }
    client->close("done");
    CHECK(client->closed() == "done");
    // The writer flushes go-away before shutting the socket down.
    for (int i = 0; i < 200 && frames_of(socket->output()).size() < 5; ++i)
        std::this_thread::sleep_for(std::chrono::milliseconds(5));
    const auto out = socket->output();
    CHECK(out == read_golden("cpp-client.bin"));
    const auto frames = frames_of(out);
    REQUIRE(frames.size() == 5);
    CHECK(frames[0].first == Header::window_update(1, 0, SYN));
    CHECK(frames[4].first == Header::go_away(GO_AWAY_NORMAL));

    // Rust yamux 0.14 decodes it (tests/interop `yamux-decode` at ecc0cf0).
    if (const char* rust = std::getenv("LOA_P2P_RUST_NODE")) {
        const auto path = std::string(LOA_GOLDEN_DIR) + "/p2p/yamux/cpp-client.bin";
        FILE* p = popen((std::string(rust) + " yamux-decode " + path).c_str(), "r");
        REQUIRE(p);
        std::string text;
        char line[256];
        while (std::fgets(line, sizeof line, p)) text += line;
        CHECK(pclose(p) == 0);
        CHECK(text.starts_with("STREAM 1 0070696e67\n"));
    }
}

TEST_CASE("yamux sessions stream past the 256 KiB window both ways") {
    int fds[2];
    REQUIRE(::socketpair(AF_UNIX, SOCK_STREAM, 0, fds) == 0);
    auto client = Session::start(std::make_shared<FdSocket>(fds[0]), Mode::client);
    auto server = Session::start(std::make_shared<FdSocket>(fds[1]), Mode::server);

    std::string big(1024 * 1024 + 17, '\0');
    for (std::size_t i = 0; i < big.size(); ++i) big[i] = static_cast<char>(i * 31 % 251);
    std::thread echo([&] {
        auto s = server->accept();
        REQUIRE(s.has_value());
        const auto got = read_all(*s->recv);
        CHECK(got == big);
        REQUIRE(s->send->write_all(std::span(reinterpret_cast<const std::uint8_t*>(got.data()), got.size())).has_value());
        REQUIRE(s->send->shutdown().has_value());
    });
    auto out = client->open();
    REQUIRE(out.has_value());
    std::thread writer([&] {
        REQUIRE(out->send->write_all(std::span(reinterpret_cast<const std::uint8_t*>(big.data()), big.size())).has_value());
        REQUIRE(out->send->shutdown().has_value());
    });
    CHECK(read_all(*out->recv) == big);
    writer.join();
    echo.join();

    // Dropping both halves of an open stream resets it at the peer.
    {
        auto a = client->open();
        REQUIRE(a.has_value());
        REQUIRE(a->send->write_all(Bytes{1}).has_value());
        auto b = server->accept();
        REQUIRE(b.has_value());
        std::uint8_t one = 0;
        REQUIRE(b->recv->read_exact(std::span(&one, 1)).has_value());
        a = {};
        std::uint8_t buf[1];
        CHECK_FALSE(b->recv->read(buf).has_value());
    }

    client->close("bye");
    CHECK(server->closed() == "connection is closed");
    CHECK_FALSE(server->accept().has_value());
    CHECK_FALSE(client->open().has_value());
}
