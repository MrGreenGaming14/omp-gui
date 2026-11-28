#pragma once

// `Node` (Rust `loa_p2p::node`): the facade the app drives. It owns the
// transports and composes a `PeerDirectory`, a `GrantService` and the session
// share. Like the transports and protocols, every call blocks; background work
// (presence, renewal, mDNS) runs on the node's own threads.

#include <chrono>
#include <condition_variable>
#include <cstdint>
#include <deque>
#include <filesystem>
#include <expected>
#include <functional>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <thread>
#include <utility>
#include <variant>
#include <vector>

#include "loa/p2p/auth.hpp"
#include "loa/p2p/discovery.hpp"
#include "loa/p2p/peers_db.hpp"
#include "loa/p2p/protocols/extra.hpp"
#include "loa/p2p/protocols/folder.hpp"
#include "loa/p2p/protocols/host.hpp"
#include "loa/p2p/protocols/session.hpp"
#include "loa/p2p/quic.hpp"
#include "loa/p2p/sessions.hpp"
#include "loa/trust/keystore.hpp"
#include "loa/trust/renew.hpp"

namespace loa::bus {
class Journal;
class Store;
} // namespace loa::bus

namespace loa::p2p {

// How the panel names `resource::Sandbox`.
inline constexpr std::string_view SANDBOX_LABEL = "Sandbox on this node";
// How long one dial or control request may take.
inline constexpr std::chrono::milliseconds CALL_TIMEOUT{5000};
// How long QUIC may take before TCP is tried.
inline constexpr std::chrono::milliseconds QUIC_TIMEOUT{400};
// Unread events each subscriber may hold before it lags.
inline constexpr std::size_t EVENT_CAPACITY = 256;

// The protocol labels this node announces in its hello.
[[nodiscard]] std::vector<std::string> local_protocols();

// Default directory of the node's stores inside the app data directory.
[[nodiscard]] std::filesystem::path data_dir(const std::filesystem::path& app_data);

// How the node runs. `NodeConfig::make` gives the app defaults.
struct NodeConfig {
    // Where `peers.sqlite` (and with `Node::open`, `grants.sqlite`) live.
    std::filesystem::path data_dir;
    // UDP and TCP listening address; port 0 picks free ports (tests).
    SocketAddr bind;
    RelayConfig relay;
    bool mdns = true;
    // The `tailscale` executable; none when not installed or turned off.
    std::optional<std::filesystem::path> tailscale;
    // The name peers see.
    std::string name;
    std::string app_version;
    // How often peers are probed and held grants renewed.
    std::chrono::milliseconds presence_interval{30'000};
    // Held grants renew this long before they expire.
    std::int64_t renew_margin_ms = 3 * 60'000;
    // The session store forks of peers' sessions go into; null disables forking.
    std::shared_ptr<const bus::Store> sessions;

    // Binds `0.0.0.0:31415`, relay off, mDNS on, `tailscale` from `PATH`, presence every 30 s, renewal 3 min early.
    [[nodiscard]] static NodeConfig make(std::filesystem::path data_dir, std::string name, std::string app_version);
};

// State of the endpoint.
namespace endpoint {
struct Stopped {
    friend bool operator==(const Stopped&, const Stopped&) = default;
};
struct Starting {
    friend bool operator==(const Starting&, const Starting&) = default;
};
struct Listening {
    std::vector<SocketAddr> udp;
    SocketAddr tcp;
    friend bool operator==(const Listening&, const Listening&) = default;
};
struct Failed {
    std::string error;
    friend bool operator==(const Failed&, const Failed&) = default;
};
} // namespace endpoint
using Endpoint = std::variant<endpoint::Stopped, endpoint::Starting, endpoint::Listening, endpoint::Failed>;

// What the node is and how it is reachable.
struct NodeStatus {
    NodeId key;
    std::string name;
    std::string app_version;
    trust::KeySource key_source;
    Endpoint endpoint;
    Availability mdns;
    Availability tailscale;
};

// Something the app should react to.
namespace node_event {
// Peers, grants, status or remote sessions changed.
struct Changed {
    friend bool operator==(const Changed&, const Changed&) = default;
};
} // namespace node_event
using NodeEvent = std::variant<node_event::Changed, PromptRequested, AnswerRequested>;

// One subscriber of a node's events (Rust `broadcast::Receiver<NodeEvent>`).
class NodeEvents {
  public:
    struct Queue {
        std::mutex mutex;
        std::condition_variable ready;
        std::deque<NodeEvent> events;
        // Events dropped since the last `recv` because the queue was full.
        std::uint64_t lagged = 0;
    };

    explicit NodeEvents(std::shared_ptr<Queue> queue) : queue_(std::move(queue)) {}

    // The next event, or how many were skipped after falling behind; none on timeout.
    [[nodiscard]] std::optional<std::expected<NodeEvent, std::uint64_t>> recv(std::chrono::milliseconds timeout);
    // `recv` without waiting.
    [[nodiscard]] std::optional<std::expected<NodeEvent, std::uint64_t>> try_recv();

  private:
    std::shared_ptr<Queue> queue_;
};

// The running node. Hold it in a `std::shared_ptr`; every copy is the same node.
class Node final : public NodeHost, public std::enable_shared_from_this<Node> {
  public:
    // Opens the node's stores under `config.data_dir`, grants included. Nothing touches the network until `start`.
    [[nodiscard]] static Result<std::shared_ptr<Node>> open(NodeConfig config, const trust::NodeIdentity& identity,
                                                            trust::KeySource key_source,
                                                            std::shared_ptr<LocalSessions> sessions);
    // `open` over the app's grant store, shared with whatever else checks grants.
    [[nodiscard]] static Result<std::shared_ptr<Node>>
    open_with_grants(NodeConfig config, const trust::NodeIdentity& identity, trust::KeySource key_source,
                     std::shared_ptr<LocalSessions> sessions, std::shared_ptr<trust::GrantStore> grants);
    ~Node() override;

    // ---- NodeHost ----
    [[nodiscard]] NodeId key() const override { return key_; }
    [[nodiscard]] GrantService& grants() const override { return *grants_; }
    [[nodiscard]] LocalSessions& local_sessions() const override { return *sessions_; }
    [[nodiscard]] wire::Hello hello() const override;
    void observe(const NodeId& peer, const wire::Hello& hello, TransportKind kind) override;
    void changed() override { emit(node_event::Changed{}); }
    void prompt_requested(PromptRequested request) override { emit(std::move(request)); }
    void answer_requested(AnswerRequested request) override { emit(std::move(request)); }
    // QUIC first, then TCP, on the peer's address hints; `CALL_TIMEOUT` in all.
    Result<Connection> dial(const NodeId& peer, std::string_view alpn) override;
    Result<Connection> dial_any(const SocketAddr& addr, std::string_view alpn) override;

    [[nodiscard]] const NodeConfig& config() const noexcept { return config_; }
    [[nodiscard]] NodeStatus status() const;
    // The `Authorize` every session writer this node owns must use.
    [[nodiscard]] std::shared_ptr<bus::Authorize> authorizer() const { return grants_->access; }
    [[nodiscard]] const std::shared_ptr<trust::GrantStore>& grant_store() const noexcept { return grants_->store; }
    // Changes and requests, for the app's subscription.
    [[nodiscard]] NodeEvents events();

    // Binds both transports and starts discovery, presence and renewal; a running node does nothing.
    Result<> start();
    // Stops discovery and closes the endpoint.
    void stop();

    // Parses a node key: 64 hex or 52 base32 characters, or the Biscuit form `ed25519/<hex>`.
    [[nodiscard]] static Result<NodeId> parse_key(std::string_view text);

    // Saves a peer by key, with an optional `host[:port]` to reach it, then probes it.
    Result<> add_peer(std::string_view key, std::optional<std::string_view> address = std::nullopt);
    // Saves a node found on the LAN, the tailnet or that connected here.
    Result<> add_nearby(std::string_view key);
    Result<> rename_peer(std::string_view key, std::string_view name);
    // Forgets a peer. Grants given to it stay until revoked.
    Result<> remove_peer(std::string_view key);

    [[nodiscard]] std::vector<PeerInfo> peers() const { return peers_->list(); }
    [[nodiscard]] std::vector<NearbyPeer> nearby() const { return peers_->nearby(); }
    // A peer's display name: the user's, its own, or the start of its key.
    [[nodiscard]] std::string peer_name(const NodeId& key) const { return peers_->name(key); }
    [[nodiscard]] PeerDirectory& directory() const noexcept { return *peers_; }

    // Probes every saved peer, refreshing what each shares and delivering waiting grants.
    void refresh_presence();
    // Probes every online tailnet machine on this node's port; those that answer
    // show up in `nearby`. Returns how many answered.
    Result<std::size_t> scan_tailscale();

    // Grants a peer `scopes` until `until_ms` (none: until revoked) and sends
    // it the token; an offline holder gets it when it answers.
    Result<trust::GrantId> issue_grant(std::string_view holder, std::vector<trust::Scope> scopes,
                                       std::optional<std::int64_t> until_ms, bool reshare);
    // Moves the end of a grant this node issued and sends the holder a fresh token.
    Result<> renew_grant(const trust::GrantId& id, std::optional<std::int64_t> until_ms);
    // Revokes a grant this node issued and tells its holder.
    Result<> revoke_grant(const trust::GrantId& id);
    // Renews held grants that expire within the renewal margin.
    std::vector<trust::RenewOutcome> renew_due();
    // Grants this node issued, then grants it holds.
    [[nodiscard]] std::vector<GrantInfo> grant_infos() const { return grants_->infos(); }

    // Resources the app adds to grant pickers: SSH hosts, surfaces.
    void set_resources(std::vector<std::pair<trust::Resource, std::string>> resources);
    // What grants can name here, labelled: future sessions, projects, sessions
    // and prompts, the sandbox, granted folders, then extra resources.
    [[nodiscard]] std::vector<std::pair<trust::Resource, std::string>> resources() const;

    // Serves `name/version` from the next `start` to requesters proving a grant for `required`.
    void add_protocol(std::string name, std::uint32_t version, std::pair<trust::Resource, trust::Action> required,
                      std::shared_ptr<PeerProtocol> handler);
    void add(AddedProtocol protocol);
    [[nodiscard]] std::vector<AddedProtocol> added_protocols() const;
    // A stream on `name/version` to `peer` past the grant handshake.
    Result<Stream> open_stream(const NodeId& peer, std::string_view name, std::uint32_t version);

    // Online peers whose live grants to this node cover `required`.
    [[nodiscard]] std::vector<NodeId> granting_peers(const std::pair<trust::Resource, trust::Action>& required) const;
    // Peers' folders this node's live grants reach, by peer and path.
    [[nodiscard]] std::vector<SharedFolder> shared_folders() const;
    Result<std::vector<FolderEntry>> list_folder(std::string_view peer, std::string_view folder, std::string_view path);
    Result<Bytes> read_folder_file(std::string_view peer, std::string_view folder, std::string_view path);
    Result<> write_folder_file(std::string_view peer, std::string_view folder, std::string_view path,
                               std::span<const std::uint8_t> bytes);

    // Sessions of `peer` this node's grants reach, asked over the network.
    Result<std::vector<RemoteSession>> list_remote(std::string_view peer);
    // Every peer session last listed, newest first, for Recents.
    [[nodiscard]] std::vector<RemoteSession> remote_sessions() const;
    Result<std::unique_ptr<RemoteView>> open_remote(std::string_view peer, std::string_view session);
    Result<> prompt_remote(std::string_view peer, std::string_view session, std::string_view text);
    // Copies a peer's session, as far as the grant shows it, into a new session this node owns (needs fork).
    Result<std::string> fork_remote(std::string_view peer, std::string_view session);
    // Forks an open view into `store` as `new_id`.
    Result<bus::Journal> fork_view(const RemoteView& view, const bus::Store& store, std::string_view new_id) const;

  private:
    Node(NodeConfig config, NodeId key, std::unique_ptr<PeerDirectory> peers, std::unique_ptr<GrantService> grants,
         std::shared_ptr<LocalSessions> sessions, NodeStatus status);

    void emit(NodeEvent event);
    template <class F> void status_with(F&& change);
    [[nodiscard]] std::shared_ptr<FallbackDialer> network() const;
    Result<wire::Control> control_call(const NodeId& peer, wire::Control request);
    void probe(const NodeId& peer);
    void deliver(const NodeId& holder, const trust::GrantId& id, Bytes token);
    void deliver_waiting(const NodeId& peer);
    [[nodiscard]] std::vector<GrantInfo> live_received() const;
    Result<> known(std::string_view key, const std::function<bool(const NodeId&)>& change);

    NodeConfig config_;
    NodeId key_;
    std::unique_ptr<PeerDirectory> peers_;
    std::unique_ptr<GrantService> grants_;
    std::shared_ptr<LocalSessions> sessions_;

    mutable std::mutex remote_mutex_;
    std::map<NodeId, std::vector<RemoteSession>> remote_;

    // Serializes `start` and `stop`.
    std::mutex lifecycle_;
    mutable std::mutex net_mutex_;
    std::shared_ptr<FallbackDialer> net_;

    mutable std::mutex state_mutex_;
    NodeStatus status_;
    std::vector<std::pair<trust::Resource, std::string>> resources_;
    // Protocols the app added; served from the next start.
    std::vector<AddedProtocol> added_;

    // Background threads: presence and renewal, mDNS discovery.
    struct Tasks;
    std::shared_ptr<Tasks> tasks_;

    std::mutex events_mutex_;
    std::vector<std::weak_ptr<NodeEvents::Queue>> subscribers_;
};

} // namespace loa::p2p
