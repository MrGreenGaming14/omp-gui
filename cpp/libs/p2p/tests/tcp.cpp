#include <doctest/doctest.h>

#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>
#include <unistd.h>

#include <openssl/ssl.h>

#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstdlib>
#include <deque>
#include <mutex>

#include "loa/iroh_ffi/iroh.hpp"
#include "loa/p2p/quic.hpp"
#include "loa/p2p/tcp.hpp"
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

std::uint16_t port_of(const SocketAddr& addr) { return static_cast<std::uint16_t>(std::stoi(addr.substr(addr.rfind(':') + 1))); }

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

} // namespace

TEST_CASE("node certificate: a self-signed Ed25519 leaf whose SPKI is the node key") {
    auto der = node_certificate(seed(1));
    REQUIRE(der.has_value());
    auto id = certificate_id(*der);
    REQUIRE(id.has_value());
    CHECK(*id == key(1));
    // The SPKI is the 12-byte prefix followed by the key.
    Bytes spki(ED25519_SPKI_PREFIX.begin(), ED25519_SPKI_PREFIX.end());
    const auto node = key(1);
    spki.insert(spki.end(), node.as_bytes().begin(), node.as_bytes().end());
    CHECK(std::search(der->begin(), der->end(), spki.begin(), spki.end()) != der->end());
    CHECK(certificate_id(Bytes{1, 2, 3}).error().kind() == NodeError::Kind::invalid_key);
}

TEST_CASE("tcp loopback: streams both ways, wrong key and label refused") {
    auto capture = std::make_shared<Capture>();
    Router router;
    router.handler(ALPN, capture);
    auto server = Tcp::bind(seed(1), "127.0.0.1:0", router);
    auto client = Tcp::bind(seed(2), "127.0.0.1:0", Router{});
    REQUIRE(server.has_value());
    REQUIRE(client.has_value());
    const auto hints = (*server)->local_addrs();
    REQUIRE(hints.size() == 1);
    CHECK(hints[0].starts_with("127.0.0.1:"));
    CHECK(port_of(hints[0]) != 0);

    auto dialed = (*client)->dial(key(1), hints, ALPN);
    REQUIRE(dialed.has_value());
    CHECK((*dialed)->remote() == key(1));
    CHECK((*dialed)->kind() == TransportKind::tcp);
    CHECK((*dialed)->alpn() == Bytes(ALPN.begin(), ALPN.end()));
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
    REQUIRE_FALSE(wrong.has_value()); // a different key is refused
    CHECK(wrong.error().kind() == NodeError::Kind::connect);
    CHECK(wrong.error().message().find("remote node key differs") != std::string::npos);
    CHECK_FALSE((*client)->dial(key(1), hints, "omp/test/2").has_value()); // labels match exactly
    auto any = (*client)->dial_any(hints[0], ALPN);
    REQUIRE(any.has_value());
    CHECK((*any)->remote() == key(1));

    (*dialed)->close(7, "done");
    CHECK((*dialed)->closed() == "7: done");
    CHECK(incoming->closed() == "connection is closed");
    CHECK_FALSE(incoming->accept_stream().has_value());
    (*client)->close();
    (*server)->close();
    CHECK_FALSE((*client)->dial(key(1), hints, ALPN).has_value()); // the listener stopped
}

TEST_CASE("tcp refuses a TLS 1.2 client and a client without a certificate") {
    Router router;
    router.handler(ALPN, std::make_shared<Capture>());
    auto server = Tcp::bind(seed(1), "127.0.0.1:0", router);
    REQUIRE(server.has_value());
    const auto port = port_of((*server)->local_addrs()[0]);

    const auto attempt = [&](int max_version) {
        SSL_CTX* ctx = SSL_CTX_new(TLS_client_method());
        SSL_CTX_set_max_proto_version(ctx, max_version);
        SSL_CTX_set_verify(ctx, SSL_VERIFY_NONE, nullptr);
        const int fd = ::socket(AF_INET, SOCK_STREAM, 0);
        sockaddr_in addr{};
        addr.sin_family = AF_INET;
        addr.sin_port = htons(port);
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
        REQUIRE(::connect(fd, reinterpret_cast<sockaddr*>(&addr), sizeof addr) == 0);
        SSL* ssl = SSL_new(ctx);
        SSL_set_fd(ssl, fd);
        const unsigned char protos[] = "\x0aomp/test/1";
        SSL_set_alpn_protos(ssl, protos, sizeof protos - 1);
        bool ok = SSL_connect(ssl) == 1;
        if (ok) {
            // TLS 1.3: the server judges the (missing) client certificate after our Finished.
            char byte;
            ok = SSL_read(ssl, &byte, 1) > 0;
        }
        SSL_free(ssl);
        ::close(fd);
        SSL_CTX_free(ctx);
        return ok;
    };
    CHECK_FALSE(attempt(TLS1_2_VERSION));
    CHECK_FALSE(attempt(TLS1_3_VERSION));
    (*server)->close();
}

TEST_CASE("falls back to tcp when udp does not answer") {
    auto capture = std::make_shared<Capture>();
    Router router;
    router.handler(ALPN, capture);
    auto tcp = Tcp::bind(seed(1), "127.0.0.1:0", router);
    auto quic = Quic::bind(seed(2), "127.0.0.1:0", RelayConfig::off(), Router{});
    auto client = Tcp::bind(seed(2), "127.0.0.1:0", Router{});
    REQUIRE(tcp.has_value());
    REQUIRE(quic.has_value());
    REQUIRE(client.has_value());
    const int blocked = ::socket(AF_INET, SOCK_DGRAM, 0);
    sockaddr_in addr{};
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    REQUIRE(::bind(blocked, reinterpret_cast<sockaddr*>(&addr), sizeof addr) == 0);
    socklen_t len = sizeof addr;
    ::getsockname(blocked, reinterpret_cast<sockaddr*>(&addr), &len);
    const std::vector<SocketAddr> hints{std::format("127.0.0.1:{}", ntohs(addr.sin_port)), (*tcp)->local_addrs()[0]};

    FallbackDialer dialer{{*quic, *client}, 400ms};
    auto conn = dialer.dial(key(1), hints, ALPN);
    REQUIRE(conn.has_value());
    CHECK((*conn)->kind() == TransportKind::tcp);
    CHECK(capture->wait(5s) != nullptr);
    ::close(blocked);
    // Not `dialer.close()`: the timed-out QUIC attempt still runs detached, and closing
    // its endpoint waits for it; the Rust test leaves every transport open too.
    (*client)->close();
    (*tcp)->close();
}

// The Rust side is tests/interop (see the QUIC interop test in transport.cpp):
// `tcp-serve` serves an echoing omp/control/1 over TCP on seed [1; 32], and
// `tcp-dial <key> <addr>` dials one over TCP on seed [2; 32].
TEST_CASE("interop: a C++ node and a Rust node exchange omp/control/1 over the TCP fallback") {
    const char* rust = std::getenv("LOA_P2P_RUST_NODE");
    if (!rust) return;
    const wire::ControlFrame hello{wire::Control(wire::Hello{"0.1.0", {{"omp/control", 1}}, "cpp"})};

    // C++ dials Rust.
    FILE* server = popen((std::string(rust) + " tcp-serve").c_str(), "r");
    REQUIRE(server);
    char line[256] = {};
    REQUIRE(std::fgets(line, sizeof line, server));
    std::string addr(line);
    REQUIRE(addr.starts_with("ADDR "));
    addr = addr.substr(5, addr.find_last_not_of("\r\n") - 4);
    auto client = Tcp::bind(seed(2), "127.0.0.1:0", Router{});
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
    std::system("pkill -f 'loa-interop tcp-serve'");
    pclose(server);

    // Rust dials C++.
    Router router;
    router.handler(wire::CONTROL, std::make_shared<Echo>());
    auto node = Tcp::bind(seed(1), "127.0.0.1:0", router);
    REQUIRE(node.has_value());
    FILE* dialer =
        popen((std::string(rust) + " tcp-dial " + key(1).to_string() + " " + (*node)->local_addrs()[0]).c_str(), "r");
    REQUIRE(dialer);
    std::string out;
    while (std::fgets(line, sizeof line, dialer)) out += line;
    CHECK(pclose(dialer) == 0);
    CHECK(out.starts_with("RUST DIAL OK"));
    (*node)->close();
}
