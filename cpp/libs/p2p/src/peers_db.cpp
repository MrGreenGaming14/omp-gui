#include "loa/p2p/peers_db.hpp"

#include <sqlite3.h>

#include <algorithm>
#include <format>

#include "loa/runtime/json.hpp"
#include "loa/runtime/time.hpp"

namespace loa::p2p {

namespace json = loa::runtime::json;

const std::string_view PeerDirectory::SCHEMA =
    "CREATE TABLE IF NOT EXISTS net_peers (key TEXT PRIMARY KEY NOT NULL, name TEXT, addresses TEXT NOT NULL DEFAULT "
    "'[]',\n             discovery_source TEXT NOT NULL, online INTEGER NOT NULL DEFAULT 0, last_seen_ms INTEGER, "
    "app_version TEXT);";

namespace {

constexpr std::string_view COLUMNS =
    "SELECT key, name, addresses, discovery_source, online, last_seen_ms, app_version FROM net_peers";

NodeError db_error(sqlite3* db) { return NodeError(NodeError::Kind::io, std::string("sqlite: ") + sqlite3_errmsg(db)); }

// A prepared statement, finalized on scope exit.
struct Statement {
    sqlite3_stmt* stmt = nullptr;
    Statement(sqlite3* db, std::string_view sql) {
        if (sqlite3_prepare_v2(db, sql.data(), static_cast<int>(sql.size()), &stmt, nullptr) != SQLITE_OK) stmt = nullptr;
    }
    ~Statement() { sqlite3_finalize(stmt); }
    Statement(const Statement&) = delete;
    Statement& operator=(const Statement&) = delete;

    void bind(int index, const SqlValue& value) const {
        if (const auto* n = std::get_if<std::int64_t>(&value)) sqlite3_bind_int64(stmt, index, *n);
        else if (const auto* s = std::get_if<std::string>(&value))
            sqlite3_bind_text(stmt, index, s->data(), static_cast<int>(s->size()), SQLITE_TRANSIENT);
        else sqlite3_bind_null(stmt, index);
    }
};

std::optional<std::string> text_column(sqlite3_stmt* stmt, int i) {
    if (sqlite3_column_type(stmt, i) == SQLITE_NULL) return std::nullopt;
    const auto* text = reinterpret_cast<const char*>(sqlite3_column_text(stmt, i));
    return std::string(text, static_cast<std::size_t>(sqlite3_column_bytes(stmt, i)));
}

// One `COLUMNS` row; none when a column doesn't parse (Rust skips such rows).
std::optional<PeerInfo> row(sqlite3_stmt* stmt) {
    auto key_text = text_column(stmt, 0);
    auto addresses_text = text_column(stmt, 2);
    auto source_text = text_column(stmt, 3);
    if (!key_text || !addresses_text || !source_text) return std::nullopt;
    auto key = trust::PublicKey::parse(*key_text);
    auto source = parse_discovery_source(*source_text);
    auto parsed = json::parse(*addresses_text);
    if (!key || !source || !parsed || !parsed->as_array()) return std::nullopt;
    PeerInfo peer;
    peer.key = *key;
    peer.name = text_column(stmt, 1);
    for (const auto& address : *parsed->as_array()) {
        const auto* text = address.as_string();
        if (!text) return std::nullopt;
        peer.addresses.push_back(*text);
    }
    peer.source = *source;
    peer.online = sqlite3_column_int64(stmt, 4) != 0;
    if (sqlite3_column_type(stmt, 5) != SQLITE_NULL) peer.last_seen_ms = sqlite3_column_int64(stmt, 5);
    peer.app_version = text_column(stmt, 6);
    return peer;
}

} // namespace

std::string_view to_string(DiscoverySource source) noexcept {
    switch (source) {
    case DiscoverySource::manual: return "manual";
    case DiscoverySource::mdns: return "mdns";
    case DiscoverySource::tailscale: return "tailscale";
    }
    return "manual";
}

std::optional<DiscoverySource> parse_discovery_source(std::string_view text) noexcept {
    if (text == "manual") return DiscoverySource::manual;
    if (text == "mdns") return DiscoverySource::mdns;
    if (text == "tailscale") return DiscoverySource::tailscale;
    return std::nullopt;
}

std::string short_key(const NodeId& key) { return key.to_string().substr(0, 10); }

std::string addresses_json(std::span<const SocketAddr> addresses) {
    json::Array array;
    for (const auto& address : addresses) array.emplace_back(address);
    return json::dump(json::Value(std::move(array)));
}

Result<std::unique_ptr<PeerDirectory>> PeerDirectory::open(const std::filesystem::path& path) {
    sqlite3* db = nullptr;
    if (sqlite3_open(path.string().c_str(), &db) != SQLITE_OK) {
        auto error = db_error(db);
        sqlite3_close(db);
        return std::unexpected(error);
    }
    sqlite3_busy_timeout(db, 5000);
    if (sqlite3_exec(db, std::string(SCHEMA).c_str(), nullptr, nullptr, nullptr) != SQLITE_OK) {
        auto error = db_error(db);
        sqlite3_close(db);
        return std::unexpected(error);
    }
    return std::unique_ptr<PeerDirectory>(new PeerDirectory(db));
}

PeerDirectory::~PeerDirectory() { sqlite3_close(db_); }

Result<> PeerDirectory::save(const NodeId& key, std::optional<std::string_view> name,
                             std::span<const SocketAddr> addresses, DiscoverySource source) {
    {
        std::scoped_lock lock(db_mutex_);
        Statement s(db_, "INSERT INTO net_peers (key, name, addresses, discovery_source) VALUES (?1, ?2, ?3, ?4)\n"
                         "             ON CONFLICT(key) DO UPDATE SET name = COALESCE(excluded.name, net_peers.name),\n"
                         "               addresses = CASE WHEN excluded.addresses = '[]' THEN net_peers.addresses ELSE "
                         "excluded.addresses END,\n"
                         "               discovery_source = excluded.discovery_source");
        if (!s.stmt) return std::unexpected(db_error(db_));
        s.bind(1, key.to_string());
        s.bind(2, name ? SqlValue(std::string(*name)) : SqlValue());
        s.bind(3, addresses_json(addresses));
        s.bind(4, std::string(to_string(source)));
        if (sqlite3_step(s.stmt) != SQLITE_DONE) return std::unexpected(db_error(db_));
    }
    std::scoped_lock lock(seen_mutex_);
    seen_.nearby.erase(key);
    return {};
}

Result<> PeerDirectory::save_nearby(const NodeId& key) {
    std::optional<NearbyPeer> nearby;
    {
        std::scoped_lock lock(seen_mutex_);
        if (auto it = seen_.nearby.find(key); it != seen_.nearby.end()) nearby = it->second;
    }
    if (!nearby) return fail(NodeError::Kind::unknown_peer, "unknown peer " + key.to_string());
    DiscoverySource source = DiscoverySource::manual;
    switch (nearby->source) {
    case NearbySource::mdns: source = DiscoverySource::mdns; break;
    case NearbySource::tailscale: source = DiscoverySource::tailscale; break;
    case NearbySource::incoming: source = DiscoverySource::manual; break;
    }
    std::optional<std::string_view> name;
    if (nearby->name) name = *nearby->name;
    if (auto saved = save(key, name, nearby->addresses, source); !saved) return saved;
    if (nearby->app_version) {
        const SqlValue values[] = {*nearby->app_version};
        update(key, "app_version = ?2", values);
    }
    return {};
}

std::optional<PeerInfo> PeerDirectory::lookup(const NodeId& key) const {
    std::scoped_lock lock(db_mutex_);
    Statement s(db_, std::string(COLUMNS) + " WHERE key = ?1");
    if (!s.stmt) return std::nullopt;
    s.bind(1, key.to_string());
    if (sqlite3_step(s.stmt) != SQLITE_ROW) return std::nullopt;
    return row(s.stmt);
}

std::vector<PeerInfo> PeerDirectory::list() const {
    std::vector<PeerInfo> peers;
    {
        std::scoped_lock lock(db_mutex_);
        Statement s(db_, std::string(COLUMNS) + " ORDER BY key");
        if (!s.stmt) return {};
        while (sqlite3_step(s.stmt) == SQLITE_ROW)
            if (auto peer = row(s.stmt)) peers.push_back(std::move(*peer));
    }
    std::scoped_lock lock(seen_mutex_);
    for (auto& peer : peers) {
        if (auto it = seen_.transports.find(peer.key); it != seen_.transports.end() && peer.online)
            peer.transport = it->second;
        if (auto it = seen_.protocols.find(peer.key); it != seen_.protocols.end()) {
            peer.protocols = it->second;
            peer.shared_protocol = std::ranges::find(peer.protocols, wire::SESSION) != peer.protocols.end();
        }
    }
    return peers;
}

std::vector<NodeId> PeerDirectory::keys() const {
    std::vector<NodeId> out;
    for (auto& peer : list()) out.push_back(peer.key);
    return out;
}

std::vector<NearbyPeer> PeerDirectory::nearby() const {
    std::vector<NearbyPeer> out;
    {
        std::scoped_lock lock(seen_mutex_);
        for (const auto& [_, peer] : seen_.nearby) out.push_back(peer);
    }
    std::ranges::sort(out, [](const NearbyPeer& a, const NearbyPeer& b) {
        if (a.seen_ms != b.seen_ms) return a.seen_ms > b.seen_ms;
        return a.key < b.key;
    });
    return out;
}

bool PeerDirectory::update(const NodeId& key, std::string_view set, std::span<const SqlValue> values) {
    std::scoped_lock lock(db_mutex_);
    Statement s(db_, std::format("UPDATE net_peers SET {} WHERE key = ?1", set));
    if (!s.stmt) return false;
    s.bind(1, key.to_string());
    for (std::size_t i = 0; i < values.size(); ++i) s.bind(static_cast<int>(i) + 2, values[i]);
    return sqlite3_step(s.stmt) == SQLITE_DONE && sqlite3_changes(db_) != 0;
}

bool PeerDirectory::rename(const NodeId& key, std::string_view name) {
    const SqlValue values[] = {std::string(name)};
    return update(key, "name = ?2", values);
}

bool PeerDirectory::remove(const NodeId& key) {
    {
        std::scoped_lock lock(seen_mutex_);
        seen_.transports.erase(key);
    }
    std::scoped_lock lock(db_mutex_);
    Statement s(db_, "DELETE FROM net_peers WHERE key = ?1");
    if (!s.stmt) return false;
    s.bind(1, key.to_string());
    return sqlite3_step(s.stmt) == SQLITE_DONE && sqlite3_changes(db_) != 0;
}

void PeerDirectory::clear_transports() {
    std::scoped_lock lock(seen_mutex_);
    seen_.transports.clear();
}

void PeerDirectory::offline(const NodeId& key) {
    update(key, "online = 0", {});
    std::scoped_lock lock(seen_mutex_);
    seen_.transports.erase(key);
}

std::string PeerDirectory::name(const NodeId& key) const {
    if (auto saved = lookup(key); saved && saved->name && !saved->name->empty()) return *saved->name;
    {
        std::scoped_lock lock(seen_mutex_);
        if (auto it = seen_.nearby.find(key); it != seen_.nearby.end() && it->second.name) return *it->second.name;
    }
    return short_key(key);
}

std::vector<SocketAddr> PeerDirectory::hints(const NodeId& key) const {
    std::vector<SocketAddr> saved;
    if (auto peer = lookup(key)) saved = std::move(peer->addresses);
    std::vector<SocketAddr> nearby;
    {
        std::scoped_lock lock(seen_mutex_);
        if (auto it = seen_.nearby.find(key); it != seen_.nearby.end()) nearby = it->second.addresses;
    }
    return merge_addresses(nearby, std::move(saved), false);
}

void PeerDirectory::found(NearbyPeer peer) {
    if (auto saved = lookup(peer.key)) {
        auto addresses = merge_addresses(saved->addresses, std::move(peer.addresses), peer.source == NearbySource::mdns);
        const SqlValue values[] = {addresses_json(addresses)};
        update(peer.key, "addresses = ?2", values);
        return;
    }
    std::scoped_lock lock(seen_mutex_);
    if (auto it = seen_.nearby.find(peer.key); it != seen_.nearby.end()) it->second.merge(std::move(peer));
    else seen_.nearby.emplace(peer.key, std::move(peer));
}

void PeerDirectory::observe(const NodeId& peer, const wire::Hello& hello, TransportKind kind) {
    std::vector<std::string> labels;
    for (const auto& p : hello.protocols) labels.push_back(std::format("{}/{}", p.name, p.version));
    const SqlValue values[] = {hello.app_version, runtime::time::now_ms(), hello.name};
    const bool known = update(peer,
                              "app_version = ?2, online = 1, last_seen_ms = ?3, name = CASE WHEN name IS NULL OR name "
                              "= '' THEN NULLIF(?4, '') ELSE name END",
                              values);
    {
        std::scoped_lock lock(seen_mutex_);
        seen_.transports[peer] = kind;
        seen_.protocols[peer] = labels;
    }
    if (!known) {
        auto sighting = NearbyPeer::make(peer, NearbySource::incoming,
                                         hello.name.empty() ? std::nullopt : std::optional(hello.name), {});
        sighting.app_version = hello.app_version;
        sighting.protocols = std::move(labels);
        found(std::move(sighting));
    }
}

} // namespace loa::p2p
