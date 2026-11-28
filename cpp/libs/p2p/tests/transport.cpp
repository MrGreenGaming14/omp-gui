#include <doctest/doctest.h>

#include <chrono>
#include <condition_variable>
#include <deque>
#include <mutex>
#include <cstdio>
#include <cstdlib>
#include <thread>

#include "loa/iroh_ffi/iroh.hpp"
#include "loa/p2p/quic.hpp"
#include "loa/p2p/wire.hpp"

using namespace loa::p2p;
using namespace std::chrono_literals;

namespace {

constexpr std::string_view ALPN = "omp/test/1";

std::array<std::uint8_t, 32> seed(std::uint8_t b) {
    std::array<std::uint8_t, 32> s;
    s.fill(b);
    return s;
}
NodeId key(std::uint8_t b) { return NodeId{loa::iroh_ffi::endpoint_id_from_seed(seed(b))}; }

// Collects accepted connections.
class Capture final : public ProtocolHandler {
  public:
    void handle(Connection conn) override {
        std::lock_guard lock(mutex_);
        conns_.push_back(std::move(conn));
        cv_.notify_all();
    }
    Connection wait(std::chrono::milliseconds timeout) {
        std::unique_lock lock(mutex_);
        if (!cv_.wait_for(lock, timeout, [&] { return !conns_.empty(); })) return nullptr;
        auto c = conns_.front();
        conns_.pop_front();
        return c;
    }

  private:
    std::mutex mutex_;
    std::condition_variable cv_;
    std::deque<Connection> conns_;
};

class FakeConnection final : public PeerConnection {
  public:
    explicit FakeConnection(std::string alpn, TransportKind kind = TransportKind::tcp)
        : alpn_(alpn.begin(), alpn.end()), kind_(kind) {}
    NodeId remote() const override { return key(1); }
    Bytes alpn() const override { return alpn_; }
    TransportKind kind() const override { return kind_; }
    Result<Stream> open_stream() override { return fail(NodeError::Kind::net, "fake"); }
    Result<Stream> accept_stream() override { return fail(NodeError::Kind::net, "fake"); }
    void close(std::uint32_t code, std::string_view reason) override {
        close_code = code;
        close_reason = reason;
    }
    std::string closed() override { return close_reason; }

    std::optional<std::uint32_t> close_code;
    std::string close_reason;

  private:
    Bytes alpn_;
    TransportKind kind_;
};

class FakeTransport final : public Transport {
  public:
    FakeTransport(std::chrono::milliseconds delay, bool ok, TransportKind kind) : delay_(delay), ok_(ok), kind_(kind) {}
    Result<Connection> dial(const NodeId&, std::span<const SocketAddr>, std::string_view alpn) override {
        ++dials;
        std::this_thread::sleep_for(delay_);
        if (!ok_) return fail(NodeError::Kind::net, "unreachable");
        return Connection(std::make_shared<FakeConnection>(std::string(alpn), kind_));
    }
    Result<Connection> dial_any(const SocketAddr&, std::string_view alpn) override { return dial(key(0), {}, alpn); }
    std::vector<SocketAddr> local_addrs() const override { return {}; }
    void close() override { closed = true; }

    std::atomic<int> dials{0};
    std::atomic<bool> closed{false};

  private:
    std::chrono::milliseconds delay_;
    bool ok_;
    TransportKind kind_;
};

std::vector<SocketAddr> loopback_hints(const Transport& t) {
    std::vector<SocketAddr> out;
    for (auto& a : t.local_addrs())
        if (a.starts_with("127.0.0.1:")) out.push_back(a);
    return out;
}

std::string read_to_end(RecvHalf& rx) {
    std::string out;
    std::uint8_t buf[256];
    for (;;) {
        auto n = rx.read(buf);
        REQUIRE(n.has_value());
        if (*n == 0) return out;
        out.append(reinterpret_cast<const char*>(buf), *n);
    }
}

} // namespace

TEST_CASE("router dispatches by exact label and closes unknown ones with 0x404") {
    auto capture = std::make_shared<Capture>();
    Router router;
    router.handler(ALPN, capture);
    CHECK(router.alpns() == std::vector<std::string>{std::string(ALPN)});

    auto served = std::make_shared<FakeConnection>(std::string(ALPN));
    router.dispatch(served);
    CHECK(capture->wait(0ms) == served);
    CHECK_FALSE(served->close_code.has_value());

    auto unknown = std::make_shared<FakeConnection>("omp/test/2");
    router.dispatch(unknown);
    CHECK(unknown->close_code == std::optional<std::uint32_t>(CLOSE_NO_HANDLER));
    CHECK(unknown->close_reason == "no handler");
    CHECK(capture->wait(0ms) == nullptr);
}

TEST_CASE("check_alpn requires the exact label") {
    const Bytes same(ALPN.begin(), ALPN.end());
    const Bytes other{'o', 'm', 'p'};
    CHECK(check_alpn(key(1), ALPN, &same).has_value());
    CHECK(check_alpn(key(1), ALPN, &other).error().kind() == NodeError::Kind::connect);
    CHECK_FALSE(check_alpn(key(1), ALPN, nullptr).has_value());
}

TEST_CASE("fallback dialer times out the first transport, then falls through in order") {
    auto slow = std::make_shared<FakeTransport>(2s, true, TransportKind::quic);
    auto failing = std::make_shared<FakeTransport>(0ms, false, TransportKind::quic);
    auto tcp = std::make_shared<FakeTransport>(0ms, true, TransportKind::tcp);
    FallbackDialer dialer{{slow, failing, tcp}, 100ms};
    const auto started = std::chrono::steady_clock::now();
    auto conn = dialer.dial(key(1), {}, ALPN);
    REQUIRE(conn.has_value());
    CHECK((*conn)->kind() == TransportKind::tcp);
    CHECK(std::chrono::steady_clock::now() - started < 1500ms);
    CHECK(failing->dials == 1);

    FallbackDialer none{{failing}, 100ms};
    CHECK(none.dial(key(1), {}, ALPN).error().message() == "unreachable");
    FallbackDialer empty{{}, 100ms};
    CHECK(empty.dial_any("127.0.0.1:1", ALPN).error().kind() == NodeError::Kind::unavailable);
    dialer.close();
    CHECK((slow->closed && failing->closed && tcp->closed));
}

TEST_CASE("quic loopback: streams both ways, wrong key and label refused") {
    auto capture = std::make_shared<Capture>();
    Router router;
    router.handler(ALPN, capture);
    auto server = Quic::bind(seed(1), "127.0.0.1:0", RelayConfig::off(), router);
    auto client = Quic::bind(seed(2), "127.0.0.1:0", RelayConfig::off(), Router{});
    REQUIRE(server.has_value());
    REQUIRE(client.has_value());
    const auto hints = loopback_hints(**server);
    REQUIRE_FALSE(hints.empty());

    auto dialed = (*client)->dial(key(1), hints, ALPN);
    REQUIRE(dialed.has_value());
    CHECK((*dialed)->remote() == key(1));
    CHECK((*dialed)->kind() == TransportKind::quic);
    auto incoming = capture->wait(5s);
    REQUIRE(incoming);
    CHECK(incoming->remote() == key(2));

    auto out = (*dialed)->open_stream();
    REQUIRE(out.has_value());
    REQUIRE(out->send->write_all(Bytes{'p', 'i', 'n', 'g'}).has_value());
    REQUIRE(out->send->shutdown().has_value());
    auto in = incoming->accept_stream();
    REQUIRE(in.has_value());
    std::string buf = read_to_end(*in->recv);
    REQUIRE(in->send->write_all(Bytes{'p', 'o', 'n', 'g'}).has_value());
    REQUIRE(in->send->shutdown().has_value());
    buf += read_to_end(*out->recv);
    CHECK(buf == "pingpong");

    auto wrong = (*client)->dial(key(9), hints, ALPN);
    CHECK_FALSE(wrong.has_value()); // a different key is refused
    CHECK_FALSE((*client)->dial(key(1), hints, "omp/test/2").has_value()); // labels match exactly
    CHECK((*client)->dial_any(hints[0], ALPN).error().kind() == NodeError::Kind::invalid_address);
    (*dialed)->close(0, "done");
    (*client)->close();
    (*server)->close();
}

TEST_CASE("quic carries one omp/control/1 hello exchange") {
    struct Echo final : ProtocolHandler {
        void handle(Connection conn) override {
            auto s = conn->accept_stream();
            if (!s) return;
            auto hello = wire::recv<wire::ControlFrame>(*s->recv);
            if (!hello || !*hello) return;
            (void)wire::send(*s->send, **hello);
            (void)s->send->shutdown();
            conn->closed();
        }
    };
    Router router;
    router.handler(wire::CONTROL, std::make_shared<Echo>());
    auto server = Quic::bind(seed(3), "127.0.0.1:0", RelayConfig::self_hosted_at("http://127.0.0.1:9/"), router);
    auto client = Quic::bind(seed(4), "127.0.0.1:0", RelayConfig::off(), Router{});
    REQUIRE(server.has_value());
    REQUIRE(client.has_value());
    auto conn = (*client)->dial(key(3), loopback_hints(**server), wire::CONTROL);
    REQUIRE(conn.has_value());
    auto stream = (*conn)->open_stream();
    REQUIRE(stream.has_value());
    const wire::ControlFrame hello{wire::Control(wire::Hello{"0.1.0", {{"omp/control", 1}}, "cpp"})};
    auto answer = wire::call<wire::ControlFrame>(*stream, hello);
    REQUIRE(answer.has_value());
    CHECK(*answer == hello);
    (*conn)->close(0, "done");
    (*client)->close();
    (*server)->close();
}

TEST_CASE("quic bind refuses a malformed relay URL") {
    CHECK(Quic::bind(seed(5), "127.0.0.1:0", RelayConfig::self_hosted_at("not a url"), Router{}).error().kind() ==
          NodeError::Kind::invalid_address);
}

// Interop with a Rust node (`cpp/libs/p2p/tests/interop` at ecc0cf0, the last commit with the
// Rust workspace: copy the root Cargo.lock in, `cargo build`), then set LOA_P2P_RUST_NODE to the binary. It
// serves an echoing omp/control/1 on seed [1; 32] and prints
// `ADDR <addr>`, and with `dial <key> <addr>` dials one and prints `RUST DIAL OK`.
TEST_CASE("interop: a C++ node and a Rust node exchange omp/control/1 both ways") {
    const char* rust = std::getenv("LOA_P2P_RUST_NODE");
    if (!rust) return;
    const wire::ControlFrame hello{wire::Control(wire::Hello{"0.1.0", {{"omp/control", 1}}, "cpp"})};

    // C++ dials Rust.
    FILE* server = popen((std::string(rust) + " serve").c_str(), "r");
    REQUIRE(server);
    char line[256] = {};
    REQUIRE(std::fgets(line, sizeof line, server));
    std::string addr(line);
    REQUIRE(addr.starts_with("ADDR "));
    addr = addr.substr(5, addr.find_last_not_of("\r\n") - 4);
    auto client = Quic::bind(seed(2), "127.0.0.1:0", RelayConfig::off(), Router{});
    REQUIRE(client.has_value());
    auto conn = (*client)->dial(key(1), std::vector<SocketAddr>{addr}, wire::CONTROL);
    REQUIRE(conn.has_value());
    auto stream = (*conn)->open_stream();
    REQUIRE(stream.has_value());
    auto answer = wire::call<wire::ControlFrame>(*stream, hello);
    REQUIRE(answer.has_value());
    CHECK(std::get<wire::Hello>(*answer->body).name == "rust");
    (*conn)->close(0, "done");
    (*client)->close();
    std::system("pkill -f 'loa-interop serve'");
    pclose(server);

    // Rust dials C++.
    struct Echo final : ProtocolHandler {
        void handle(Connection c) override {
            auto s = c->accept_stream();
            if (!s) return;
            auto got = wire::recv<wire::ControlFrame>(*s->recv);
            if (!got || !*got) return;
            (void)wire::send(*s->send, **got);
            (void)s->send->shutdown();
            c->closed();
        }
    };
    Router router;
    router.handler(wire::CONTROL, std::make_shared<Echo>());
    auto node = Quic::bind(seed(3), "127.0.0.1:0", RelayConfig::off(), router);
    REQUIRE(node.has_value());
    const auto hints = loopback_hints(**node);
    REQUIRE_FALSE(hints.empty());
    FILE* dialer = popen((std::string(rust) + " dial " + key(3).to_string() + " " + hints[0]).c_str(), "r");
    REQUIRE(dialer);
    std::string out;
    while (std::fgets(line, sizeof line, dialer)) out += line;
    CHECK(pclose(dialer) == 0);
    CHECK(out.starts_with("RUST DIAL OK"));
    (*node)->close();
}
