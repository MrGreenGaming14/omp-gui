#include "loa/p2p/node.hpp"

#include <arpa/inet.h>
#include <netdb.h>
#include <sys/socket.h>

#include <asio.hpp>

#include <algorithm>
#include <charconv>
#include <format>
#include <set>
#include <system_error>

#include "loa/bus/journal.hpp"
#include "loa/bus/store.hpp"
#include "loa/p2p/protocols/control.hpp"
#include "loa/p2p/protocols/projection.hpp"
#include "loa/p2p/tcp.hpp"
#include "loa/runtime/exec.hpp"
#include "loa/runtime/log.hpp"
#include "loa/runtime/time.hpp"

namespace loa::p2p {

using runtime::time::now_ms;

namespace {

std::optional<std::uint16_t> parse_port(std::string_view text) {
    std::uint16_t port = 0;
    auto [end, ec] = std::from_chars(text.data(), text.data() + text.size(), port);
    if (ec != std::errc{} || end != text.data() + text.size() || text.empty()) return std::nullopt;
    return port;
}

// The port of a socket address in display form.
std::uint16_t port_of(const SocketAddr& addr) {
    auto colon = addr.rfind(':');
    if (colon == std::string::npos) return 0;
    return parse_port(std::string_view(addr).substr(colon + 1)).value_or(0);
}

// Resolves `host[:port]`; the port defaults to `DEFAULT_PORT`.
Result<std::vector<SocketAddr>> resolve(std::string_view address) {
    const auto invalid = [&](std::string reason) {
        return fail(NodeError::Kind::invalid_address, std::format("invalid address \"{}\": {}", address, reason));
    };
    std::string host(address);
    std::uint16_t port = DEFAULT_PORT;
    if (auto colon = address.rfind(':'); colon != std::string_view::npos) {
        if (auto p = parse_port(address.substr(colon + 1))) {
            port = *p;
            host = std::string(address.substr(0, colon));
        }
    }
    if (host.size() >= 2 && host.front() == '[' && host.back() == ']') host = host.substr(1, host.size() - 2);
    addrinfo hints{};
    hints.ai_family = AF_UNSPEC;
    hints.ai_socktype = SOCK_STREAM;
    addrinfo* found = nullptr;
    if (int rc = getaddrinfo(host.c_str(), std::to_string(port).c_str(), &hints, &found); rc != 0)
        return invalid(gai_strerror(rc));
    std::vector<SocketAddr> out;
    for (auto* ai = found; ai; ai = ai->ai_next) {
        char ip[INET6_ADDRSTRLEN] = {};
        SocketAddr text;
        if (ai->ai_family == AF_INET) {
            inet_ntop(AF_INET, &reinterpret_cast<sockaddr_in*>(ai->ai_addr)->sin_addr, ip, sizeof ip);
            text = std::format("{}:{}", ip, port);
        } else if (ai->ai_family == AF_INET6) {
            inet_ntop(AF_INET6, &reinterpret_cast<sockaddr_in6*>(ai->ai_addr)->sin6_addr, ip, sizeof ip);
            text = std::format("[{}]:{}", ip, port);
        } else {
            continue;
        }
        if (std::ranges::find(out, text) == out.end()) out.push_back(std::move(text));
    }
    freeaddrinfo(found);
    if (out.empty()) return invalid("no addresses");
    return out;
}

// Runs `work` on its own thread and waits at most `timeout`; a late result is
// handed to `late` (e.g. to close a connection nobody waits for any more).
template <class T, class Work, class Late>
Result<T> with_timeout(std::chrono::milliseconds timeout, std::string what, Work work, Late late) {
    struct Shared {
        std::mutex mutex;
        std::condition_variable done;
        std::optional<Result<T>> result;
        bool abandoned = false;
    };
    auto shared = std::make_shared<Shared>();
    std::thread([shared, work = std::move(work), late = std::move(late)]() mutable {
        auto result = work();
        std::unique_lock lock(shared->mutex);
        if (shared->abandoned) {
            lock.unlock();
            late(result);
            return;
        }
        shared->result = std::move(result);
        shared->done.notify_all();
    }).detach();
    std::unique_lock lock(shared->mutex);
    if (!shared->done.wait_for(lock, timeout, [&] { return shared->result.has_value(); })) {
        shared->abandoned = true;
        return fail(NodeError::Kind::timeout, std::move(what));
    }
    return std::move(*shared->result);
}

Result<> create_dir(const std::filesystem::path& dir) {
    std::error_code ec;
    std::filesystem::create_directories(dir, ec);
    if (ec) return fail(NodeError::Kind::unavailable, std::format("{}: {}", dir.string(), ec.message()));
    return {};
}

bool is_listening(const Endpoint& endpoint) { return std::holds_alternative<endpoint::Listening>(endpoint); }

} // namespace

// ---- events ----

std::optional<std::expected<NodeEvent, std::uint64_t>> NodeEvents::recv(std::chrono::milliseconds timeout) {
    std::unique_lock lock(queue_->mutex);
    if (!queue_->ready.wait_for(lock, timeout, [&] { return queue_->lagged != 0 || !queue_->events.empty(); }))
        return std::nullopt;
    if (queue_->lagged != 0) return std::unexpected(std::exchange(queue_->lagged, 0));
    auto event = std::move(queue_->events.front());
    queue_->events.pop_front();
    return event;
}

std::optional<std::expected<NodeEvent, std::uint64_t>> NodeEvents::try_recv() {
    return recv(std::chrono::milliseconds(0));
}

// ---- config ----

std::vector<std::string> local_protocols() {
    std::vector<std::string> out;
    for (auto label : wire::LOCAL) out.emplace_back(label);
    return out;
}

std::filesystem::path data_dir(const std::filesystem::path& app_data) { return app_data / "node"; }

NodeConfig NodeConfig::make(std::filesystem::path data_dir, std::string name, std::string app_version) {
    NodeConfig config;
    config.data_dir = std::move(data_dir);
    config.bind = std::format("0.0.0.0:{}", DEFAULT_PORT);
    config.relay = RelayConfig::off();
    config.mdns = true;
    config.tailscale = runtime::exec::which("tailscale");
    config.name = std::move(name);
    config.app_version = std::move(app_version);
    return config;
}

// ---- node ----

struct Node::Tasks {
    std::mutex mutex;
    std::condition_variable wake;
    bool stopping = false;
    std::thread presence;
    std::unique_ptr<Mdns> mdns;
    std::thread discover;

    // Stops and joins every thread; one running on the calling thread is detached.
    void stop() {
        {
            std::scoped_lock lock(mutex);
            stopping = true;
        }
        wake.notify_all();
        if (mdns) mdns->stop();
        for (auto* thread : {&presence, &discover}) {
            if (!thread->joinable()) continue;
            if (thread->get_id() == std::this_thread::get_id()) thread->detach();
            else thread->join();
        }
        // The Mdns goes with the last owner: a detached thread may still be in `next`.
    }
};

Node::Node(NodeConfig config, NodeId key, std::unique_ptr<PeerDirectory> peers, std::unique_ptr<GrantService> grants,
           std::shared_ptr<LocalSessions> sessions, NodeStatus status)
    : config_(std::move(config)), key_(key), peers_(std::move(peers)), grants_(std::move(grants)),
      sessions_(std::move(sessions)), status_(std::move(status)) {}

Node::~Node() {
    if (tasks_) tasks_->stop();
    if (net_) net_->close();
}

Result<std::shared_ptr<Node>> Node::open(NodeConfig config, const trust::NodeIdentity& identity,
                                         trust::KeySource key_source, std::shared_ptr<LocalSessions> sessions) {
    if (auto made = create_dir(config.data_dir); !made) return std::unexpected(made.error());
    auto store = trust::GrantStore::open(config.data_dir / "grants.sqlite");
    if (!store) return std::unexpected(grant_error(store.error()));
    return open_with_grants(std::move(config), identity, std::move(key_source), std::move(sessions),
                            std::make_shared<trust::GrantStore>(std::move(*store)));
}

Result<std::shared_ptr<Node>> Node::open_with_grants(NodeConfig config, const trust::NodeIdentity& identity,
                                                     trust::KeySource key_source,
                                                     std::shared_ptr<LocalSessions> sessions,
                                                     std::shared_ptr<trust::GrantStore> grants) {
    if (auto made = create_dir(config.data_dir); !made) return std::unexpected(made.error());
    const NodeId key = identity.public_key();
    Availability tailscale;
    if (config.tailscale)
        tailscale = {Availability::State::active,
                     std::format("{} found; scan to look for nodes", config.tailscale->string())};
    else if (runtime::exec::which("tailscale"))
        tailscale = {Availability::State::off, "turned off"};
    else
        tailscale = {Availability::State::off, "tailscale is not installed"};
    Availability mdns{Availability::State::off, config.mdns ? "starts with the endpoint" : "turned off"};
    NodeStatus status{key, config.name, config.app_version, std::move(key_source), endpoint::Stopped{}, mdns, tailscale};
    auto peers = PeerDirectory::open(config.data_dir / "peers.sqlite");
    if (!peers) return std::unexpected(peers.error());
    auto service = std::make_unique<GrantService>(std::move(grants), identity, config.renew_margin_ms);
    return std::shared_ptr<Node>(new Node(std::move(config), key, std::move(*peers), std::move(service),
                                          std::move(sessions), std::move(status)));
}

void Node::emit(NodeEvent event) {
    std::scoped_lock lock(events_mutex_);
    std::erase_if(subscribers_, [](const auto& weak) { return weak.expired(); });
    for (const auto& weak : subscribers_) {
        auto queue = weak.lock();
        if (!queue) continue;
        {
            std::scoped_lock q(queue->mutex);
            if (queue->events.size() >= EVENT_CAPACITY) {
                queue->events.pop_front();
                ++queue->lagged;
            }
            queue->events.push_back(event);
        }
        queue->ready.notify_all();
    }
}

NodeEvents Node::events() {
    auto queue = std::make_shared<NodeEvents::Queue>();
    std::scoped_lock lock(events_mutex_);
    subscribers_.push_back(queue);
    return NodeEvents(std::move(queue));
}

NodeStatus Node::status() const {
    std::scoped_lock lock(state_mutex_);
    return status_;
}

template <class F> void Node::status_with(F&& change) {
    {
        std::scoped_lock lock(state_mutex_);
        change(status_);
    }
    changed();
}

std::shared_ptr<FallbackDialer> Node::network() const {
    std::scoped_lock lock(net_mutex_);
    return net_;
}

wire::Hello Node::hello() const {
    wire::Hello hello{config_.app_version, {}, config_.name};
    for (const auto& label : local_protocols()) {
        auto slash = label.rfind('/');
        if (slash == std::string::npos) continue;
        std::uint32_t version = 0;
        auto digits = std::string_view(label).substr(slash + 1);
        auto [end, ec] = std::from_chars(digits.data(), digits.data() + digits.size(), version);
        if (ec != std::errc{} || end != digits.data() + digits.size()) continue;
        hello.protocols.push_back({label.substr(0, slash), version});
    }
    return hello;
}

void Node::observe(const NodeId& peer, const wire::Hello& hello, TransportKind kind) {
    peers_->observe(peer, hello, kind);
    changed();
}

Result<Connection> Node::dial(const NodeId& peer, std::string_view alpn) {
    auto net = network();
    if (!net) return fail(NodeError::Kind::not_started, "the peer endpoint is not running");
    auto hints = peers_->hints(peer);
    return with_timeout<Connection>(
        CALL_TIMEOUT, "dialing the peer",
        [net, peer, hints = std::move(hints), alpn = std::string(alpn)] { return net->dial(peer, hints, alpn); },
        [](Result<Connection>& late) {
            if (late) (*late)->close(0, "timed out");
        });
}

Result<Connection> Node::dial_any(const SocketAddr& addr, std::string_view alpn) {
    auto net = network();
    if (!net) return fail(NodeError::Kind::not_started, "the peer endpoint is not running");
    return net->dial_any(addr, alpn);
}

Result<wire::Control> Node::control_call(const NodeId& peer, wire::Control request) {
    auto conn = dial(peer, wire::CONTROL);
    if (!conn) return std::unexpected(conn.error());
    auto self = shared_from_this();
    auto answer = with_timeout<wire::Control>(
        CALL_TIMEOUT, "the control request",
        [self, conn = *conn, request = std::move(request)]() mutable { return control::call(*self, conn, std::move(request)); },
        [](Result<wire::Control>&) {});
    (*conn)->close(0, "done");
    return answer;
}

Result<> Node::start() {
    std::scoped_lock lifecycle(lifecycle_);
    if (network()) return {};
    status_with([](NodeStatus& s) { s.endpoint = endpoint::Starting{}; });
    std::weak_ptr<NodeHost> weak = shared_from_this();
    Router router;
    router.handler(wire::CONTROL, served(weak, control::serve));
    router.handler(wire::SESSION, served(weak, session::serve));
    router.handler(wire::FOLDER, served(weak, folder::serve));
    for (auto& added : added_protocols())
        router.handler(added.label(), served(weak, [added](NodeHost& host, const Connection& conn, Stream stream) {
                           return extra::serve(host, conn, std::move(stream), added);
                       }));
    const auto& seed = grants_->identity.secret_bytes();
    auto failed = [&](const NodeError& error) -> Result<> {
        status_with([&](NodeStatus& s) { s.endpoint = endpoint::Failed{error.to_string()}; });
        return std::unexpected(error);
    };
    auto quic = Quic::bind(seed, config_.bind, config_.relay, router);
    if (!quic) return failed(quic.error());
    auto tcp = Tcp::bind(seed, config_.bind, std::move(router));
    if (!tcp) {
        (*quic)->close();
        return failed(tcp.error());
    }
    auto udp = (*quic)->local_addrs();
    auto tcp_addr = (*tcp)->local_addrs().at(0);
    auto net = std::make_shared<FallbackDialer>();
    net->transports = {*quic, *tcp};
    net->first_timeout = QUIC_TIMEOUT;
    {
        std::scoped_lock lock(net_mutex_);
        net_ = net;
    }

    auto tasks = std::make_shared<Tasks>();
    std::weak_ptr<Node> self = shared_from_this();
    tasks->presence = std::thread([self, raw = tasks, every = config_.presence_interval] {
        while (true) {
            if (auto node = self.lock()) {
                node->refresh_presence();
                node->renew_due();
            } else {
                return;
            }
            std::unique_lock lock(raw->mutex);
            if (raw->wake.wait_for(lock, every, [&] { return raw->stopping; })) return;
        }
    });
    Availability mdns{Availability::State::off, "turned off"};
    if (config_.mdns) {
        Announce announce{key_, udp.empty() ? std::uint16_t{0} : port_of(udp.front()), port_of(tcp_addr), config_.name};
        if (auto started = Mdns::start(announce)) {
            mdns = (*started)->availability();
            tasks->mdns = std::move(*started);
            tasks->discover = std::thread([self, keep = tasks, discoverer = tasks->mdns.get()] {
                while (auto found = discoverer->next()) {
                    auto node = self.lock();
                    if (!node) return;
                    node->peers_->found(std::move(*found));
                    node->changed();
                }
            });
        } else {
            mdns = {Availability::State::failed, started.error().to_string()};
        }
    }
    tasks_ = std::move(tasks);
    status_with([&](NodeStatus& s) {
        s.endpoint = endpoint::Listening{std::move(udp), std::move(tcp_addr)};
        s.mdns = std::move(mdns);
    });
    return {};
}

void Node::stop() {
    std::scoped_lock lifecycle(lifecycle_);
    if (auto tasks = std::move(tasks_)) tasks->stop();
    std::shared_ptr<FallbackDialer> net;
    {
        std::scoped_lock lock(net_mutex_);
        net = std::exchange(net_, nullptr);
    }
    if (net) net->close();
    peers_->clear_transports();
    status_with([](NodeStatus& s) {
        s.endpoint = endpoint::Stopped{};
        if (s.mdns.state == Availability::State::active) s.mdns = {Availability::State::off, "stopped with the endpoint"};
    });
}

Result<NodeId> Node::parse_key(std::string_view text) {
    while (!text.empty() && std::isspace(static_cast<unsigned char>(text.front()))) text.remove_prefix(1);
    while (!text.empty() && std::isspace(static_cast<unsigned char>(text.back()))) text.remove_suffix(1);
    auto bare = text.starts_with("ed25519/") ? text.substr(8) : text;
    auto key = trust::PublicKey::parse(bare);
    if (!key) return fail(NodeError::Kind::invalid_key, std::format("not a node key: \"{}\"", text));
    return *key;
}

Result<> Node::add_peer(std::string_view key_text, std::optional<std::string_view> address) {
    auto key = parse_key(key_text);
    if (!key) return std::unexpected(key.error());
    if (*key == key_) return fail(NodeError::Kind::invalid_key, "not a node key: \"this node's own key\"");
    std::vector<SocketAddr> addresses;
    std::string_view trimmed = address.value_or("");
    while (!trimmed.empty() && std::isspace(static_cast<unsigned char>(trimmed.front()))) trimmed.remove_prefix(1);
    while (!trimmed.empty() && std::isspace(static_cast<unsigned char>(trimmed.back()))) trimmed.remove_suffix(1);
    if (!trimmed.empty()) {
        auto resolved = resolve(trimmed);
        if (!resolved) return std::unexpected(resolved.error());
        addresses = std::move(*resolved);
    } else {
        addresses = peers_->hints(*key);
    }
    std::optional<std::string> name;
    for (auto& nearby : peers_->nearby())
        if (nearby.key == *key) name = nearby.name;
    std::optional<std::string_view> name_view;
    if (name) name_view = *name;
    if (auto saved = peers_->save(*key, name_view, addresses, DiscoverySource::manual); !saved) return saved;
    changed();
    probe(*key);
    return {};
}

Result<> Node::add_nearby(std::string_view key_text) {
    auto key = parse_key(key_text);
    if (!key) return std::unexpected(key.error());
    if (auto saved = peers_->save_nearby(*key); !saved) return saved;
    changed();
    probe(*key);
    return {};
}

Result<> Node::known(std::string_view key_text, const std::function<bool(const NodeId&)>& change) {
    auto key = parse_key(key_text);
    if (!key) return std::unexpected(key.error());
    if (!change(*key)) return fail(NodeError::Kind::unknown_peer, "unknown peer " + key->to_string());
    changed();
    return {};
}

Result<> Node::rename_peer(std::string_view key, std::string_view name) {
    while (!name.empty() && std::isspace(static_cast<unsigned char>(name.front()))) name.remove_prefix(1);
    while (!name.empty() && std::isspace(static_cast<unsigned char>(name.back()))) name.remove_suffix(1);
    return known(key, [&](const NodeId& id) { return peers_->rename(id, name); });
}

Result<> Node::remove_peer(std::string_view key) {
    return known(key, [&](const NodeId& id) {
        {
            std::scoped_lock lock(remote_mutex_);
            remote_.erase(id);
        }
        return peers_->remove(id);
    });
}

void Node::refresh_presence() {
    std::vector<std::thread> probes;
    for (const auto& peer : peers_->keys())
        probes.emplace_back([self = shared_from_this(), peer] { self->probe(peer); });
    for (auto& t : probes) t.join();
}

void Node::probe(const NodeId& peer) {
    if (auto answered = control_call(peer, control::presence()); !answered) {
        runtime::log::debug("p2p", std::format("peer {} unreachable: {}", peer.to_string(), answered.error().to_string()));
        peers_->offline(peer);
        return changed();
    }
    deliver_waiting(peer);
    if (grants_->holds_from(peer)) {
        (void)list_remote(peer.to_string());
        return;
    }
    bool removed = false;
    {
        std::scoped_lock lock(remote_mutex_);
        removed = remote_.erase(peer) != 0;
    }
    // Its grants ended: its sessions leave Recents.
    if (removed) changed();
}

Result<std::size_t> Node::scan_tailscale() {
    if (!config_.tailscale) return fail(NodeError::Kind::unavailable, "tailscale is not installed");
    std::uint16_t port = port_of(config_.bind);
    if (port == 0) {
        auto endpoint = status().endpoint;
        const auto* listening = std::get_if<endpoint::Listening>(&endpoint);
        if (!listening) return fail(NodeError::Kind::not_started, "the peer endpoint is not running");
        port = port_of(listening->tcp);
    }
    Tailnet tailnet(*config_.tailscale, port, std::make_shared<control::Probe>(shared_from_this()));
    std::size_t count = 0;
    while (auto found = tailnet.next()) {
        if (found->key == key_) continue;
        peers_->found(std::move(*found));
        ++count;
    }
    auto availability = tailnet.availability();
    if (availability.state == Availability::State::failed) {
        status_with([&](NodeStatus& s) { s.tailscale = availability; });
        return fail(NodeError::Kind::unavailable, availability.detail);
    }
    status_with([&](NodeStatus& s) {
        s.tailscale = {Availability::State::active,
                       std::format("last scan found {} node{}", count, count == 1 ? "" : "s")};
    });
    return count;
}

Result<trust::GrantId> Node::issue_grant(std::string_view holder_text, std::vector<trust::Scope> scopes,
                                         std::optional<std::int64_t> until_ms, bool reshare) {
    auto holder = parse_key(holder_text);
    if (!holder) return std::unexpected(holder.error());
    const auto now = now_ms();
    const auto ttl_ms = until_ms ? std::min(*until_ms - now, trust::DEFAULT_TTL_MS) : trust::DEFAULT_TTL_MS;
    auto issued = grants_->store->issue(grants_->identity, *holder,
                                        trust::GrantSpec{std::move(scopes), ttl_ms, reshare, until_ms}, now);
    if (!issued) return std::unexpected(grant_error(issued.error()));
    changed();
    deliver(*holder, issued->id, std::move(issued->token));
    return issued->id;
}

Result<> Node::renew_grant(const trust::GrantId& id, std::optional<std::int64_t> until_ms) {
    auto issued = grants_->store->extend(grants_->identity, id, until_ms, now_ms());
    if (!issued) return std::unexpected(grant_error(issued.error()));
    changed();
    deliver(issued->holder, id, std::move(issued->token));
    return {};
}

Result<> Node::revoke_grant(const trust::GrantId& id) {
    if (auto revoked = grants_->store->revoke(grants_->key(), id, now_ms()); !revoked)
        return std::unexpected(grant_error(revoked.error()));
    {
        std::scoped_lock lock(grants_->undelivered_mutex);
        grants_->undelivered.erase(id);
    }
    changed();
    auto issued = grants_->store->issued();
    if (!issued) return std::unexpected(grant_error(issued.error()));
    for (const auto& record : *issued) {
        if (record.id != id) continue;
        const auto& text = id.as_str();
        auto told = control_call(record.holder, wire::GrantRevocation{Bytes(text.begin(), text.end())});
        if (!told)
            runtime::log::debug("p2p", std::format("revocation notice to {} not delivered; the grant ends at its next "
                                                   "renewal: {}",
                                                   record.holder.to_string(), told.error().to_string()));
        break;
    }
    return {};
}

void Node::deliver(const NodeId& holder, const trust::GrantId& id, Bytes token) {
    auto delivered = control_call(holder, wire::GrantDelivery{std::move(token), {}});
    std::scoped_lock lock(grants_->undelivered_mutex);
    if (delivered) {
        grants_->undelivered.erase(id);
    } else {
        runtime::log::debug("p2p", std::format("grant delivery to {} deferred until the holder answers: {}",
                                               holder.to_string(), delivered.error().to_string()));
        grants_->undelivered.insert(id);
    }
}

void Node::deliver_waiting(const NodeId& peer) {
    std::set<trust::GrantId> waiting;
    {
        std::scoped_lock lock(grants_->undelivered_mutex);
        waiting = grants_->undelivered;
    }
    if (waiting.empty()) return;
    auto issued = grants_->store->issued();
    if (!issued) return;
    for (const auto& record : *issued) {
        if (record.holder != peer || !waiting.contains(record.id) || record.revoked_ms) continue;
        auto fresh = grants_->store->extend(grants_->identity, record.id, record.until_ms, now_ms());
        if (fresh) {
            deliver(peer, record.id, std::move(fresh->token));
        } else {
            runtime::log::debug("p2p", std::format("waiting grant {} can no longer be delivered", record.id.as_str()));
            std::scoped_lock lock(grants_->undelivered_mutex);
            grants_->undelivered.erase(record.id);
        }
    }
}

std::vector<trust::RenewOutcome> Node::renew_due() {
    trust::Renewer renewer(grants_->key(), config_.renew_margin_ms);
    control::RenewVia via(shared_from_this());
    std::vector<trust::RenewOutcome> outcomes;
    asio::io_context io;
    asio::co_spawn(
        io,
        [&]() -> asio::awaitable<void> {
            auto renewed = co_await renewer.renew_due(*grants_->store, via, now_ms());
            if (renewed) outcomes = std::move(*renewed);
        },
        asio::detached);
    io.run();
    if (!outcomes.empty()) changed();
    return outcomes;
}

void Node::set_resources(std::vector<std::pair<trust::Resource, std::string>> resources) {
    {
        std::scoped_lock lock(state_mutex_);
        resources_ = std::move(resources);
    }
    changed();
}

std::vector<std::pair<trust::Resource, std::string>> Node::resources() const {
    auto sessions = sessions_->sessions();
    std::vector<std::string> projects;
    for (const auto& session : sessions)
        if (session.facts.project) projects.push_back(*session.facts.project);
    std::ranges::sort(projects);
    projects.erase(std::unique(projects.begin(), projects.end()), projects.end());
    std::vector<std::pair<trust::Resource, std::string>> out;
    out.emplace_back(trust::resource::FutureSessions{0}, "Every future session");
    for (auto& id : projects) out.emplace_back(trust::resource::Project{id}, "Project " + id);
    for (const auto& session : sessions) {
        auto label = session.label();
        out.emplace_back(trust::resource::Session{session.facts.id}, label);
        for (std::size_t i = 0; i < session.turns.size(); ++i) {
            const auto& turn = session.turns[i];
            out.emplace_back(trust::resource::Prompt{session.facts.id, turn.id},
                             std::format("{} › prompt {}: {}", label, i + 1, turn.preview));
        }
    }
    std::scoped_lock lock(state_mutex_);
    if (std::ranges::any_of(added_, [](const AddedProtocol& protocol) {
            return std::holds_alternative<trust::resource::Sandbox>(protocol.required.first);
        }))
        out.emplace_back(trust::resource::Sandbox{}, std::string(SANDBOX_LABEL));
    // Folders already granted, to grant again; others are typed into the panel.
    std::vector<std::string> folders;
    if (auto issued = grants_->store->issued()) {
        for (const auto& record : *issued) {
            if (record.revoked_ms || !record.scopes) continue;
            for (const auto& scope : *record.scopes)
                if (const auto* folder = std::get_if<trust::resource::Folder>(&scope.resource))
                    folders.push_back(folder->path);
        }
    }
    std::ranges::sort(folders);
    folders.erase(std::unique(folders.begin(), folders.end()), folders.end());
    for (auto& path : folders) out.emplace_back(trust::resource::Folder{path}, "Folder " + path);
    out.insert(out.end(), resources_.begin(), resources_.end());
    return out;
}

void Node::add_protocol(std::string name, std::uint32_t version, std::pair<trust::Resource, trust::Action> required,
                        std::shared_ptr<PeerProtocol> handler) {
    add(AddedProtocol{std::move(name), version, std::move(required), std::move(handler)});
}

void Node::add(AddedProtocol protocol) {
    std::scoped_lock lock(state_mutex_);
    std::erase_if(added_, [&](const AddedProtocol& known) { return known.label() == protocol.label(); });
    added_.push_back(std::move(protocol));
}

std::vector<AddedProtocol> Node::added_protocols() const {
    std::scoped_lock lock(state_mutex_);
    return added_;
}

Result<Stream> Node::open_stream(const NodeId& peer, std::string_view name, std::uint32_t version) {
    return extra::open(*this, peer, name, version);
}

std::vector<GrantInfo> Node::live_received() const {
    const auto now = now_ms();
    std::vector<GrantInfo> out;
    for (auto& grant : grants_->infos())
        if (grant.live_received(now)) out.push_back(std::move(grant));
    return out;
}

std::vector<NodeId> Node::granting_peers(const std::pair<trust::Resource, trust::Action>& required) const {
    const auto& [resource, action] = required;
    std::set<NodeId> granting;
    for (const auto& grant : live_received())
        if (std::ranges::any_of(grant.scopes, [&](const trust::Scope& scope) {
                return scope.resource == resource && scope.actions.contains(action);
            }))
            granting.insert(grant.peer);
    std::vector<NodeId> out;
    for (const auto& peer : peers())
        if (peer.online && granting.contains(peer.key)) out.push_back(peer.key);
    return out;
}

std::vector<SharedFolder> Node::shared_folders() const {
    std::vector<SharedFolder> out;
    for (const auto& grant : live_received()) {
        for (const auto& scope : grant.scopes) {
            const auto* folder = std::get_if<trust::resource::Folder>(&scope.resource);
            if (!folder) continue;
            const bool can_write = scope.actions.contains(trust::Action::write);
            auto it = std::ranges::find_if(out, [&](const SharedFolder& known) {
                return known.peer == grant.peer && known.path == folder->path;
            });
            if (it != out.end()) it->can_write |= can_write;
            else out.push_back(SharedFolder{grant.peer, folder->path, can_write});
        }
    }
    std::ranges::sort(out, [](const SharedFolder& a, const SharedFolder& b) {
        auto ak = a.peer.to_string(), bk = b.peer.to_string();
        if (ak != bk) return ak < bk;
        return a.path < b.path;
    });
    return out;
}

Result<std::vector<FolderEntry>> Node::list_folder(std::string_view peer, std::string_view folder,
                                                   std::string_view path) {
    auto key = parse_key(peer);
    if (!key) return std::unexpected(key.error());
    return folder::list_remote(*this, *key, folder, path);
}

Result<Bytes> Node::read_folder_file(std::string_view peer, std::string_view folder, std::string_view path) {
    auto key = parse_key(peer);
    if (!key) return std::unexpected(key.error());
    return folder::read_remote(*this, *key, folder, path);
}

Result<> Node::write_folder_file(std::string_view peer, std::string_view folder, std::string_view path,
                                 std::span<const std::uint8_t> bytes) {
    auto key = parse_key(peer);
    if (!key) return std::unexpected(key.error());
    return folder::write_remote(*this, *key, folder, path, bytes);
}

Result<std::vector<RemoteSession>> Node::list_remote(std::string_view peer) {
    auto key = parse_key(peer);
    if (!key) return std::unexpected(key.error());
    auto sessions = session::list(*this, *key);
    if (!sessions) return sessions;
    {
        std::scoped_lock lock(remote_mutex_);
        remote_[*key] = *sessions;
    }
    changed();
    return sessions;
}

std::vector<RemoteSession> Node::remote_sessions() const {
    std::vector<RemoteSession> out;
    {
        std::scoped_lock lock(remote_mutex_);
        for (const auto& [_, sessions] : remote_) out.insert(out.end(), sessions.begin(), sessions.end());
    }
    std::ranges::sort(out, [](const RemoteSession& a, const RemoteSession& b) {
        if (a.created_ms != b.created_ms) return a.created_ms > b.created_ms;
        return a.id < b.id;
    });
    return out;
}

Result<std::unique_ptr<RemoteView>> Node::open_remote(std::string_view peer, std::string_view session_id) {
    auto key = parse_key(peer);
    if (!key) return std::unexpected(key.error());
    return session::open(*this, *key, session_id);
}

Result<> Node::prompt_remote(std::string_view peer, std::string_view session_id, std::string_view text) {
    auto key = parse_key(peer);
    if (!key) return std::unexpected(key.error());
    return session::prompt(*this, *key, session_id, text);
}

Result<std::string> Node::fork_remote(std::string_view peer, std::string_view session_id) {
    if (!config_.sessions)
        return fail(NodeError::Kind::unavailable, "this node has no session store to fork into");
    auto view = open_remote(peer, session_id);
    if (!view) return std::unexpected(view.error());
    auto new_id = std::format("{}-fork-{}", session_id, now_ms());
    auto forked = fork_view(**view, *config_.sessions, new_id);
    if (!forked) return std::unexpected(forked.error());
    return new_id;
}

Result<bus::Journal> Node::fork_view(const RemoteView& view, const bus::Store& store, std::string_view new_id) const {
    if (!view.can(trust::Action::fork))
        return fail(NodeError::Kind::refused, "the grant does not allow forking this session");
    auto patches = view.patches();
    const std::uint64_t at = patches.empty() ? 0 : patches.back().seq;
    return fork_mirror_to_own_node(store, patches, at, new_id, key_.to_string());
}

} // namespace loa::p2p
