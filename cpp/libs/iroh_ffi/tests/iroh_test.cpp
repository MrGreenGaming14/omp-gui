#include <doctest/doctest.h>

#include "loa/iroh_ffi/iroh.hpp"

#include <chrono>
#include <optional>
#include <string>
#include <thread>
#include <vector>

using namespace loa::iroh_ffi;
using namespace std::chrono_literals;

namespace {

constexpr std::string_view kAlpn = "loa/test/1";

Seed seed(std::uint8_t b) {
    Seed s;
    s.fill(b);
    return s;
}

EndpointOptions loopback(std::uint8_t b, std::vector<std::string> relays = {}) {
    EndpointOptions o;
    o.seed = seed(b);
    o.alpns = {std::string(kAlpn)};
    o.relay_urls = std::move(relays);
    o.ipv4_bind = "127.0.0.1:0";
    return o;
}

std::vector<std::string> hints(const Endpoint& ep) {
    std::vector<std::string> out;
    for (auto& a : ep.ip_addrs())
        if (a.starts_with("127.0.0.1:")) out.push_back(a);
    return out;
}

// Server echoes one message back on the first bi stream, then waits for close.
void exchange(Endpoint& server, Endpoint& client) {
    auto server_hints = hints(server);
    REQUIRE_FALSE(server_hints.empty());

    std::optional<std::string> got;
    std::jthread srv([&] {
        auto conn = server.accept();
        if (!conn) return;
        auto s = conn->accept_bi();
        if (!s) return;
        auto msg = s->read_to_end(1024, 5000ms);
        if (!msg) return;
        got.emplace(msg->begin(), msg->end());
        (void)s->write(*got);
        (void)s->finish();
        conn->closed();
    });

    auto conn = client.connect(server.id(), server_hints, kAlpn);
    REQUIRE(conn.has_value());
    auto alpn = conn->alpn();
    CHECK(std::string(alpn.begin(), alpn.end()) == kAlpn);
    auto s = conn->open_bi();
    REQUIRE(s.has_value());
    REQUIRE(s->write("hello iroh").has_value());
    REQUIRE(s->finish().has_value());
    auto reply = s->read_to_end(1024, 5000ms);
    REQUIRE(reply.has_value());
    CHECK(std::string(reply->begin(), reply->end()) == "hello iroh");
    CHECK_FALSE(conn->selected_path_is_relay());
    conn->close(0, "done");
    srv.join();
    REQUIRE(got.has_value());
    CHECK(*got == "hello iroh");
}

}  // namespace

TEST_CASE("endpoint id is derived from the seed") {
    auto a = endpoint_id_from_seed(seed(1));
    CHECK(a == endpoint_id_from_seed(seed(1)));
    CHECK(a != endpoint_id_from_seed(seed(2)));
    CHECK(endpoint_id_to_string(a).size() > 32);
}

TEST_CASE("loopback connect and bi-stream exchange without relay") {
    auto server = Endpoint::bind(loopback(1));
    auto client = Endpoint::bind(loopback(2));
    REQUIRE(server.has_value());
    REQUIRE(client.has_value());
    CHECK(server->id() == endpoint_id_from_seed(seed(1)));
    exchange(*server, *client);
    client->close();
    server->close();
}

TEST_CASE("custom relay URL binds and loopback uses no relay path") {
    auto server = Endpoint::bind(loopback(3, {"http://127.0.0.1:9/"}));
    auto client = Endpoint::bind(loopback(4));
    REQUIRE(server.has_value());
    REQUIRE(client.has_value());
    exchange(*server, *client);
    client->close();
    server->close();
}

TEST_CASE("invalid relay URL is rejected") {
    auto ep = Endpoint::bind(loopback(5, {"not a url"}));
    CHECK_FALSE(ep.has_value());
}

TEST_CASE("connect with the wrong endpoint id fails") {
    auto server = Endpoint::bind(loopback(6));
    auto client = Endpoint::bind(loopback(7));
    REQUIRE(server.has_value());
    REQUIRE(client.has_value());
    auto wrong = endpoint_id_from_seed(seed(8));
    auto conn = client->connect(wrong, hints(*server), kAlpn, 1500ms);
    CHECK_FALSE(conn.has_value());
    client->close();
    server->close();
}
