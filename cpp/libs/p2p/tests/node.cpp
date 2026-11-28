// The node facade and its peer directory (Rust `loa_p2p::node`, `peers_db`,
// their golden writer and `stopping_clears_peer_transports` of `two_nodes.rs`).

#include <doctest/doctest.h>

#include <sqlite3.h>

#include <array>
#include <chrono>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <thread>

#include "loa/bus/bus.hpp"
#include "loa/p2p/node.hpp"
#include "loa/runtime/json.hpp"
#include "loa/runtime/tempfile.hpp"

using namespace loa;
using namespace loa::p2p;
using namespace std::chrono_literals;
namespace fs = std::filesystem;
namespace json = loa::runtime::json;

namespace {

std::string slurp(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::stringstream buffer;
    buffer << in.rdbuf();
    return buffer.str();
}

runtime::TempDir tempdir() {
    auto dir = runtime::TempDir::create();
    REQUIRE(dir.has_value());
    return std::move(*dir);
}

NodeId key_of(std::uint8_t byte) {
    std::array<std::uint8_t, 32> seed{};
    seed.fill(byte);
    return trust::NodeIdentity::from_secret_bytes(seed).public_key();
}

// RFC 4648 base32, lowercase, unpadded: iroh's other key form.
std::string base32(const NodeId& key) {
    static constexpr char ALPHABET[] = "abcdefghijklmnopqrstuvwxyz234567";
    std::string out;
    std::uint32_t buffer = 0;
    int bits = 0;
    for (auto byte : key.bytes) {
        buffer = (buffer << 8) | byte;
        bits += 8;
        while (bits >= 5) {
            out += ALPHABET[(buffer >> (bits - 5)) & 31];
            bits -= 5;
        }
    }
    if (bits > 0) out += ALPHABET[(buffer << (5 - bits)) & 31];
    return out;
}

// Every row of `net_peers` as the Rust golden writer dumps it.
std::string dump(sqlite3* db) {
    sqlite3_stmt* st = nullptr;
    REQUIRE(sqlite3_prepare_v2(db, "SELECT * FROM net_peers ORDER BY rowid", -1, &st, nullptr) == SQLITE_OK);
    json::Array rows;
    while (sqlite3_step(st) == SQLITE_ROW) {
        json::Object row;
        for (int i = 0; i < sqlite3_column_count(st); ++i) {
            json::Value value;
            switch (sqlite3_column_type(st, i)) {
            case SQLITE_INTEGER: value = std::int64_t{sqlite3_column_int64(st, i)}; break;
            case SQLITE_TEXT: value = std::string(reinterpret_cast<const char*>(sqlite3_column_text(st, i))); break;
            default: break;
            }
            row.emplace_back(sqlite3_column_name(st, i), std::move(value));
        }
        rows.emplace_back(std::move(row));
    }
    sqlite3_finalize(st);
    return json::dump_pretty(json::Object{{"net_peers", std::move(rows)}}) + "\n";
}

wire::Hello hello_named(std::string name) {
    return wire::Hello{"0.4.2", {{"omp/control", 1}, {"omp/session", 1}}, std::move(name)};
}

struct NoSessions final : LocalSessions {
    std::vector<LocalSession> sessions() const override { return {}; }
    std::shared_ptr<bus::SessionWriter> writer(std::string_view) const override { return nullptr; }
};

NodeConfig test_config(const fs::path& dir, std::string name) {
    auto config = NodeConfig::make(dir / "node", std::move(name), "0.4.2");
    config.bind = "127.0.0.1:0";
    config.mdns = false;
    config.tailscale = std::nullopt;
    config.presence_interval = 3600s;
    return config;
}

std::shared_ptr<Node> open_node(const fs::path& dir, std::string name, bool mdns = false) {
    auto config = test_config(dir, std::move(name));
    config.mdns = mdns;
    auto identity = trust::NodeIdentity::generate();
    REQUIRE(identity.has_value());
    auto node = Node::open(std::move(config), *identity, trust::key_source::Keychain{}, std::make_shared<NoSessions>());
    REQUIRE_MESSAGE(node.has_value(), node.error().to_string());
    return *node;
}

const endpoint::Listening& listening(const NodeStatus& status) {
    const auto* l = std::get_if<endpoint::Listening>(&status.endpoint);
    REQUIRE(l != nullptr);
    return *l;
}

} // namespace

TEST_CASE("net_peers schema and rows match the Rust golden files") {
    const fs::path sqlite_dir = fs::path(LOA_GOLDEN_DIR) / "sqlite";
    CHECK(PeerDirectory::SCHEMA == slurp(sqlite_dir / "net_peers.sql"));

    auto dir = tempdir();
    auto peers = PeerDirectory::open(dir.path() / "peers.sqlite");
    REQUIRE(peers.has_value());
    const auto key = key_of(7);
    const std::vector<SocketAddr> addresses{"192.168.1.2:4433", "[fe80::1]:4433"};
    REQUIRE((*peers)->save(key, "desk", addresses, DiscoverySource::mdns).has_value());
    const SqlValue values[] = {std::int64_t{1'800'000'000'000}, std::string("0.1.0")};
    CHECK((*peers)->update(key, "online = 1, last_seen_ms = ?2, app_version = ?3", values));
    auto listed = (*peers)->list();
    REQUIRE(listed.size() == 1);
    CHECK(listed[0].addresses == addresses);
    CHECK(listed[0].last_seen_ms == 1'800'000'000'000);
    CHECK(dump((*peers)->connection()) == slurp(sqlite_dir / "net_peers.rows.json"));
}

TEST_CASE("a peers.sqlite with the Rust rows reads back") {
    auto dir = tempdir();
    const auto path = dir.path() / "peers.sqlite";
    {
        // Write the golden rows the way the Rust app stored them, without the C++ directory.
        sqlite3* db = nullptr;
        REQUIRE(sqlite3_open(path.string().c_str(), &db) == SQLITE_OK);
        auto schema = slurp(fs::path(LOA_GOLDEN_DIR) / "sqlite" / "net_peers.sql");
        REQUIRE(sqlite3_exec(db, schema.c_str(), nullptr, nullptr, nullptr) == SQLITE_OK);
        auto rows = json::parse(slurp(fs::path(LOA_GOLDEN_DIR) / "sqlite" / "net_peers.rows.json"));
        REQUIRE(rows.has_value());
        for (const auto& row : *rows->find("net_peers")->as_array()) {
            sqlite3_stmt* st = nullptr;
            REQUIRE(sqlite3_prepare_v2(db, "INSERT INTO net_peers VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", -1, &st,
                                       nullptr) == SQLITE_OK);
            int i = 1;
            for (const auto& [_, value] : *row.as_object()) {
                if (const auto* s = value.as_string()) sqlite3_bind_text(st, i, s->c_str(), -1, SQLITE_TRANSIENT);
                else if (auto n = value.as_i64()) sqlite3_bind_int64(st, i, *n);
                ++i;
            }
            REQUIRE(sqlite3_step(st) == SQLITE_DONE);
            sqlite3_finalize(st);
        }
        sqlite3_close(db);
    }
    auto peers = PeerDirectory::open(path);
    REQUIRE(peers.has_value());
    auto listed = (*peers)->list();
    REQUIRE(listed.size() == 1);
    const auto& peer = listed[0];
    CHECK(peer.key == key_of(7));
    CHECK(peer.name == "desk");
    CHECK(peer.addresses == std::vector<SocketAddr>{"192.168.1.2:4433", "[fe80::1]:4433"});
    CHECK(peer.source == DiscoverySource::mdns);
    CHECK(peer.online);
    CHECK(peer.last_seen_ms == 1'800'000'000'000);
    CHECK(peer.app_version == "0.1.0");
    // Not connected in this process: no transport, protocols unseen.
    CHECK_FALSE(peer.transport.has_value());
    CHECK_FALSE(peer.shared_protocol.has_value());
}

TEST_CASE("parse_key takes hex, base32 and the Biscuit form of the golden node key") {
    auto text = slurp(fs::path(LOA_GOLDEN_DIR) / "trust" / "node.pub");
    const auto key = key_of(7);
    auto hex = Node::parse_key(text);
    REQUIRE(hex.has_value());
    CHECK(*hex == key);
    // Displays back to the golden text byte for byte.
    CHECK(hex->to_string() + "\n" == text);
    auto b32 = base32(key);
    CHECK(b32.size() == 52);
    CHECK(Node::parse_key(b32) == key);
    CHECK(Node::parse_key("ed25519/" + key.to_string()) == key);
    CHECK(Node::parse_key("  " + key.to_string() + " ") == key);

    auto bad = Node::parse_key("not-a-key");
    REQUIRE_FALSE(bad.has_value());
    CHECK(bad.error().kind() == NodeError::Kind::invalid_key);
    CHECK(bad.error().to_string() == "not a node key: \"not-a-key\"");
    CHECK_FALSE(Node::parse_key(key.to_string().substr(0, 63)).has_value());
}

TEST_CASE("names fall back from the saved name to the nearby name to the short key") {
    auto dir = tempdir();
    auto peers = *PeerDirectory::open(dir.path() / "peers.sqlite");
    const auto key = key_of(9);
    CHECK(peers->name(key) == key.to_string().substr(0, 10));
    CHECK(short_key(key).size() == 10);

    peers->found(NearbyPeer::make(key, NearbySource::mdns, "laptop", {"10.0.0.2:31415"}));
    CHECK(peers->name(key) == "laptop");
    CHECK(peers->nearby().size() == 1);

    // Saving with no name keeps the nearby one's lookups; an empty saved name still falls back.
    REQUIRE(peers->save(key, std::nullopt, {}, DiscoverySource::manual).has_value());
    CHECK(peers->nearby().empty());
    CHECK(peers->name(key) == short_key(key));
    CHECK(peers->rename(key, "desk"));
    CHECK(peers->name(key) == "desk");
    CHECK(peers->rename(key, ""));
    CHECK(peers->name(key) == short_key(key));
}

TEST_CASE("nearby nodes are saved under their source; hints, observe and offline") {
    auto dir = tempdir();
    auto peers = *PeerDirectory::open(dir.path() / "peers.sqlite");
    const auto a = key_of(1), b = key_of(2);

    auto seen = NearbyPeer::make(a, NearbySource::tailscale, "box", {"100.64.0.1:31415"});
    seen.app_version = "0.3.0";
    peers->found(seen);
    CHECK(peers->hints(a) == std::vector<SocketAddr>{"100.64.0.1:31415"});
    REQUIRE(peers->save_nearby(a).has_value());
    auto listed = peers->list();
    REQUIRE(listed.size() == 1);
    CHECK(listed[0].source == DiscoverySource::tailscale);
    CHECK(listed[0].name == "box");
    CHECK(listed[0].app_version == "0.3.0");
    CHECK(listed[0].addresses == std::vector<SocketAddr>{"100.64.0.1:31415"});
    CHECK(peers->save_nearby(b).error().kind() == NodeError::Kind::unknown_peer);

    // A saved peer found again gets the new addresses first.
    peers->found(NearbyPeer::make(a, NearbySource::mdns, std::nullopt, {"192.168.0.5:31415"}));
    CHECK(peers->list()[0].addresses == std::vector<SocketAddr>{"192.168.0.5:31415"});

    peers->observe(a, hello_named("ignored"), TransportKind::quic);
    listed = peers->list();
    CHECK(listed[0].online);
    CHECK(listed[0].transport == TransportKind::quic);
    CHECK(listed[0].shared_protocol == true);
    CHECK(listed[0].protocols == std::vector<std::string>{"omp/control/1", "omp/session/1"});
    CHECK(listed[0].app_version == "0.4.2");
    CHECK(listed[0].name == "box");

    peers->offline(a);
    listed = peers->list();
    CHECK_FALSE(listed[0].online);
    CHECK_FALSE(listed[0].transport.has_value());
    CHECK(listed[0].last_seen_ms.has_value());

    // An unsaved node that connected here shows up as nearby.
    peers->observe(b, hello_named("guest"), TransportKind::tcp);
    auto nearby = peers->nearby();
    REQUIRE(nearby.size() == 1);
    CHECK(nearby[0].key == b);
    CHECK(nearby[0].source == NearbySource::incoming);
    CHECK(nearby[0].name == "guest");
    REQUIRE(peers->save_nearby(b).has_value());
    CHECK(peers->list().size() == 2);
    for (const auto& peer : peers->list())
        if (peer.key == b) CHECK(peer.source == DiscoverySource::manual);

    CHECK(peers->remove(b));
    CHECK_FALSE(peers->remove(b));
    CHECK(peers->keys() == std::vector<NodeId>{a});
}

TEST_CASE("node config defaults and data directory") {
    auto config = NodeConfig::make("/data/node", "me", "1.0.0");
    CHECK(config.bind == "0.0.0.0:31415");
    CHECK(config.relay == RelayConfig::off());
    CHECK(config.mdns);
    CHECK(config.presence_interval == 30s);
    CHECK(config.renew_margin_ms == 180'000);
    CHECK(config.sessions == nullptr);
    CHECK(data_dir("/app") == fs::path("/app/node"));
    CHECK(local_protocols() == std::vector<std::string>{"omp/control/1", "omp/session/1", "omp/folder/1"});
}

TEST_CASE("start twice is a no-op; stop clears the endpoint and mDNS") {
    auto dir = tempdir();
    auto node = open_node(dir.path(), "solo", true);
    CHECK(fs::exists(dir.path() / "node" / "peers.sqlite"));
    CHECK(fs::exists(dir.path() / "node" / "grants.sqlite"));
    auto status = node->status();
    CHECK(std::holds_alternative<endpoint::Stopped>(status.endpoint));
    CHECK(status.mdns == Availability{Availability::State::off, "starts with the endpoint"});
    CHECK(status.tailscale.state == Availability::State::off);
    CHECK(node->hello().protocols.size() == 3);

    auto events = node->events();
    REQUIRE(node->start().has_value());
    const auto first = listening(node->status());
    CHECK_FALSE(first.udp.empty());
    REQUIRE(node->start().has_value());
    CHECK(listening(node->status()) == first);
    auto event = events.recv(1s);
    REQUIRE(event.has_value());
    CHECK(std::holds_alternative<node_event::Changed>(**event));

    const bool mdns_ran = node->status().mdns.state == Availability::State::active;
    node->stop();
    status = node->status();
    CHECK(std::holds_alternative<endpoint::Stopped>(status.endpoint));
    CHECK(status.mdns.state != Availability::State::active);
    if (mdns_ran) CHECK(status.mdns == Availability{Availability::State::off, "stopped with the endpoint"});
    CHECK(node->dial(key_of(1), wire::CONTROL).error().kind() == NodeError::Kind::not_started);
    // And it starts again.
    REQUIRE(node->start().has_value());
    node->stop();
}

TEST_CASE("peers are added by key, renamed and removed; this node's own key is refused") {
    auto dir = tempdir();
    auto node = open_node(dir.path(), "me");
    auto own = node->add_peer(node->key().to_string());
    REQUIRE_FALSE(own.has_value());
    CHECK(own.error().kind() == NodeError::Kind::invalid_key);
    CHECK(node->add_peer("garbage").error().kind() == NodeError::Kind::invalid_key);

    // Not started: the probe fails and the peer is saved offline.
    const auto other = key_of(4);
    REQUIRE(node->add_peer("ed25519/" + other.to_string(), "127.0.0.1:4433").has_value());
    auto peers = node->peers();
    REQUIRE(peers.size() == 1);
    CHECK(peers[0].key == other);
    CHECK(peers[0].source == DiscoverySource::manual);
    CHECK(peers[0].addresses == std::vector<SocketAddr>{"127.0.0.1:4433"});
    CHECK_FALSE(peers[0].online);

    // The port defaults to 31415.
    const auto third = key_of(5);
    REQUIRE(node->add_peer(third.to_string(), " 127.0.0.1 ").has_value());
    for (const auto& peer : node->peers())
        if (peer.key == third) CHECK(peer.addresses == std::vector<SocketAddr>{"127.0.0.1:31415"});
    CHECK(node->add_peer(third.to_string(), "no such host.invalid").error().kind() ==
          NodeError::Kind::invalid_address);

    REQUIRE(node->rename_peer(other.to_string(), "  desk ").has_value());
    CHECK(node->peer_name(other) == "desk");
    auto unknown = node->rename_peer(key_of(6).to_string(), "x");
    REQUIRE_FALSE(unknown.has_value());
    CHECK(unknown.error().kind() == NodeError::Kind::unknown_peer);
    CHECK(unknown.error().to_string() == "unknown peer " + key_of(6).to_string());

    REQUIRE(node->remove_peer(other.to_string()).has_value());
    CHECK(node->remove_peer(other.to_string()).error().kind() == NodeError::Kind::unknown_peer);
    CHECK(node->peers().size() == 1);
    CHECK(node->add_nearby(key_of(8).to_string()).error().kind() == NodeError::Kind::unknown_peer);
}

TEST_CASE("two nodes meet over QUIC; stopping clears peer transports") {
    auto owner_dir = tempdir(), guest_dir = tempdir();
    auto owner = open_node(owner_dir.path(), "owner");
    auto guest = open_node(guest_dir.path(), "guest");
    REQUIRE(owner->start().has_value());
    REQUIRE(guest->start().has_value());
    const auto owner_udp = listening(owner->status()).udp.at(0);
    const auto guest_udp = listening(guest->status()).udp.at(0);

    REQUIRE(guest->add_peer(owner->key().to_string(), owner_udp).has_value());
    REQUIRE(owner->add_peer(guest->key().to_string(), guest_udp).has_value());

    auto peers = guest->peers();
    REQUIRE(peers.size() == 1);
    CHECK(peers[0].online);
    CHECK(peers[0].transport == TransportKind::quic);
    CHECK(peers[0].app_version == "0.4.2");
    CHECK(guest->peer_name(owner->key()) == "owner");
    CHECK(peers[0].shared_protocol == true);

    guest->stop();
    CHECK_FALSE(guest->peers()[0].transport.has_value());
    owner->stop();
}
