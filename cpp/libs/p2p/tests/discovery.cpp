#include <doctest/doctest.h>

#include <mutex>

#include "loa/iroh_ffi/iroh.hpp"
#include "loa/p2p/discovery.hpp"

using namespace loa::p2p;

namespace {

NodeId key(std::uint8_t b) {
    loa::iroh_ffi::Seed s;
    s.fill(b);
    return NodeId{loa::iroh_ffi::endpoint_id_from_seed(s)};
}

std::vector<std::string> labels(const std::string& name) {
    std::vector<std::string> out;
    for (std::size_t start = 0;;) {
        auto dot = name.find('.', start);
        out.push_back(name.substr(start, dot == std::string::npos ? std::string::npos : dot - start));
        if (dot == std::string::npos) return out;
        start = dot + 1;
    }
}

class Fake final : public NodeProbe {
  public:
    explicit Fake(std::optional<NodeId> answer) : answer_(answer) {}
    std::optional<NodeId> probe(const SocketAddr& address) override {
        std::lock_guard lock(mutex);
        probed.push_back(address);
        return answer_;
    }
    std::mutex mutex;
    std::vector<SocketAddr> probed;

  private:
    std::optional<NodeId> answer_;
};

} // namespace

TEST_CASE("announcement round trips without multicast") {
    const auto node = key(7);
    auto info = Announce{node, 31415, 31416, "seven"}.info();
    REQUIRE(info.has_value());
    for (const auto& label : labels(info->fullname)) CHECK(label.size() <= 63);
    CHECK(info->fullname == "loa-" + node.to_string().substr(0, 16) + "._loa._udp.local.");
    CHECK(info->ty_domain == MDNS_SERVICE_TYPE);

    auto resolved = *info;
    resolved.addresses = {"192.168.1.2", "10.0.0.1"};
    auto peer = parse_service(resolved);
    REQUIRE(peer.has_value());
    CHECK(peer->key == node);
    CHECK(peer->name == std::optional<std::string>("seven"));
    CHECK(peer->source == NearbySource::mdns);
    CHECK(peer->addresses ==
          std::vector<SocketAddr>{"10.0.0.1:31415", "192.168.1.2:31415", "10.0.0.1:31416", "192.168.1.2:31416"});

    resolved.port = 0;
    CHECK_FALSE(parse_service(resolved).has_value());
    CHECK_FALSE(Announce{node, 0, 1, ""}.info().has_value());
    CHECK_FALSE(Announce{node, 1, 0, ""}.info().has_value());
}

TEST_CASE("mDNS parse refuses incomplete records") {
    auto info = Announce{key(7), 31415, 31415, ""}.info();
    REQUIRE(info.has_value());
    CHECK(info->property("name") == nullptr);
    auto rec = *info;
    rec.addresses = {"fe80::1", "10.0.0.1"};
    auto peer = parse_service(rec);
    REQUIRE(peer.has_value());
    CHECK_FALSE(peer->name.has_value());
    CHECK(peer->addresses == std::vector<SocketAddr>{"10.0.0.1:31415", "[fe80::1]:31415"});

    auto zero_tcp = rec;
    for (auto& [k, v] : zero_tcp.txt)
        if (k == "tcp") v = "0";
    CHECK_FALSE(parse_service(zero_tcp).has_value());
    auto other = rec;
    other.ty_domain = "_other._udp.local.";
    CHECK_FALSE(parse_service(other).has_value());
    auto no_key = rec;
    std::erase_if(no_key.txt, [](const auto& p) { return p.first == "node"; });
    CHECK_FALSE(parse_service(no_key).has_value());
}

TEST_CASE("status names are parsed defensively") {
    const std::string json =
        R"({"Peer":{"a":{"DNSName":"Host.Example.ts.net.","Online":true},"b":{"DNSName":"bad name.ts.net"},"c":{"HostName":"x"},"d":{"DNSName":"off.ts.net","Online":false}}})";
    CHECK(parse_tailscale_status(json).value() == std::vector<std::string>{"host.example.ts.net"});
    CHECK_FALSE(parse_tailscale_status(R"({"Peer":[]})").has_value());
    CHECK_FALSE(parse_tailscale_status("nope").has_value());
}

TEST_CASE("only probed candidates are admitted") {
    const std::vector<SocketAddr> addresses{"127.0.0.1:43210", "127.0.0.2:43210"};
    const std::vector<std::pair<std::string, std::vector<SocketAddr>>> candidates{{"box.example.ts.net", addresses}};
    Fake denied(std::nullopt);
    CHECK(admit_candidates(candidates, denied).empty());
    CHECK(denied.probed == addresses); // every address was probed
    Fake allowed(key(3));
    auto found = admit_candidates(candidates, allowed);
    REQUIRE(found.size() == 1);
    CHECK(found[0].key == key(3));
    CHECK(found[0].name == std::optional<std::string>("box"));
    CHECK(found[0].addresses == std::vector<SocketAddr>{addresses[0]});
    CHECK(found[0].source == NearbySource::tailscale);
}

TEST_CASE("merging sightings: mDNS replaces addresses, others add") {
    CHECK(merge_addresses(std::vector<SocketAddr>{"a"}, {}, true) == std::vector<SocketAddr>{"a"});
    CHECK(merge_addresses(std::vector<SocketAddr>{"a", "b"}, {"b", "c"}, false) == std::vector<SocketAddr>{"b", "c", "a"});
    CHECK(merge_addresses(std::vector<SocketAddr>{"a"}, {"c"}, true) == std::vector<SocketAddr>{"c"});

    auto peer = NearbyPeer::make(key(1), NearbySource::tailscale, "box", {"1.1.1.1:1"});
    auto seen = NearbyPeer::make(key(1), NearbySource::incoming, std::nullopt, {"2.2.2.2:2"});
    seen.app_version = "0.1.0";
    peer.merge(seen);
    CHECK(peer.source == NearbySource::tailscale);
    CHECK(peer.name == std::optional<std::string>("box"));
    CHECK(peer.app_version == std::optional<std::string>("0.1.0"));
    CHECK(peer.addresses == std::vector<SocketAddr>{"2.2.2.2:2", "1.1.1.1:1"});
}

TEST_CASE("tailnet reports a failing executable") {
    Tailnet tailnet("/nonexistent/tailscale", 31415, std::make_shared<Fake>(std::nullopt));
    CHECK(tailnet.availability().state == Availability::State::active);
    CHECK_FALSE(tailnet.next().has_value());
    CHECK(tailnet.availability().state == Availability::State::failed);
}
