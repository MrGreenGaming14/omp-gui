#pragma once

// The `PeerDirectory` (Rust `loa_p2p::peers_db`): saved peers in
// `peers.sqlite` and what this node saw of every node it met. A sighting is a
// hint; transports prove the key.

#include <cstdint>
#include <filesystem>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <variant>
#include <vector>

#include "loa/p2p/discovery.hpp"
#include "loa/p2p/transport.hpp"
#include "loa/p2p/wire.hpp"

struct sqlite3;

namespace loa::p2p {

// How a saved peer was found.
enum class DiscoverySource { manual, mdns, tailscale };

[[nodiscard]] std::string_view to_string(DiscoverySource source) noexcept;
[[nodiscard]] std::optional<DiscoverySource> parse_discovery_source(std::string_view text) noexcept;

// A saved peer with what this node last observed about it.
struct PeerInfo {
    NodeId key;
    std::optional<std::string> name;
    std::vector<SocketAddr> addresses;
    DiscoverySource source = DiscoverySource::manual;
    bool online = false;
    std::optional<std::int64_t> last_seen_ms;
    std::optional<std::string> app_version;
    // Transport of the last connection, while online.
    std::optional<TransportKind> transport;
    // Whether it announced `omp/session/1`; none until seen.
    std::optional<bool> shared_protocol;
    // The protocol labels of its last hello.
    std::vector<std::string> protocols;

    friend bool operator==(const PeerInfo&, const PeerInfo&) = default;
};

// A bound SQL value for `PeerDirectory::update`.
using SqlValue = std::variant<std::monostate, std::int64_t, std::string>;

// The `fmt_short` form of a key: its first 5 bytes as lowercase hex.
[[nodiscard]] std::string short_key(const NodeId& key);

// The `serde_json` text of an address list, as stored in `net_peers.addresses`.
[[nodiscard]] std::string addresses_json(std::span<const SocketAddr> addresses);

class PeerDirectory {
  public:
    // The schema text executed on open, identical to the Rust directory's.
    static const std::string_view SCHEMA;

    [[nodiscard]] static Result<std::unique_ptr<PeerDirectory>> open(const std::filesystem::path& path);
    ~PeerDirectory();
    PeerDirectory(const PeerDirectory&) = delete;
    PeerDirectory& operator=(const PeerDirectory&) = delete;

    // Saves or refreshes a peer and drops it from the nearby list; a missing name or empty hints keep what was saved.
    Result<> save(const NodeId& key, std::optional<std::string_view> name, std::span<const SocketAddr> addresses,
                  DiscoverySource source);
    // Saves a nearby node under how it was found.
    Result<> save_nearby(const NodeId& key);

    // Saved peers by key, with what was seen of them.
    [[nodiscard]] std::vector<PeerInfo> list() const;
    [[nodiscard]] std::vector<NodeId> keys() const;
    // Unsaved nodes, most recently seen first.
    [[nodiscard]] std::vector<NearbyPeer> nearby() const;

    // Runs `UPDATE net_peers SET <set> WHERE key = ?1` with `values` as `?2…`; whether the peer exists.
    bool update(const NodeId& key, std::string_view set, std::span<const SqlValue> values);
    bool rename(const NodeId& key, std::string_view name);
    bool remove(const NodeId& key);
    // Discards connection transports when the local endpoint stops.
    void clear_transports();
    // A peer that stopped answering: offline, its last sighting kept.
    void offline(const NodeId& key);

    // The user's name for a peer, its announced name, or the start of its key.
    [[nodiscard]] std::string name(const NodeId& key) const;
    // Saved and nearby address hints of `key`.
    [[nodiscard]] std::vector<SocketAddr> hints(const NodeId& key) const;
    // A discovered node: a saved one gets its new addresses, any other is nearby.
    void found(NearbyPeer peer);
    // Records what a connection's hello showed about `peer`.
    void observe(const NodeId& peer, const wire::Hello& hello, TransportKind kind);

    // The connection, for golden tests and migrations; callers must not use it concurrently.
    [[nodiscard]] sqlite3* connection() const noexcept { return db_; }

  private:
    explicit PeerDirectory(sqlite3* db) : db_(db) {}
    [[nodiscard]] std::optional<PeerInfo> lookup(const NodeId& key) const;

    struct Seen {
        std::map<NodeId, NearbyPeer> nearby;
        std::map<NodeId, TransportKind> transports;
        std::map<NodeId, std::vector<std::string>> protocols;
    };

    mutable std::mutex db_mutex_;
    sqlite3* db_;
    mutable std::mutex seen_mutex_;
    Seen seen_;
};

} // namespace loa::p2p
