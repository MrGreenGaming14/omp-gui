// The Peers panel's view (`loa::view::settings::peers`) over a `p2p::Node`: the snapshot it
// shows and the effects of its intents (port of `areas/peers/adapter.rs`).

#include <algorithm>
#include <charconv>
#include <filesystem>
#include <limits>
#include <set>

#include "areas/peers/peers.hpp"
#include "loa/runtime/fs.hpp"
#include "loa/runtime/time.hpp"

namespace loa::app::areas::peers {

namespace {

template <class... F>
struct overloaded : F... {
    using F::operator()...;
};

namespace res = trust::resource;
using p2p::NodeError;

std::unexpected<NodeError> refused(std::string text) { return p2p::fail(NodeError::Kind::refused, std::move(text)); }

std::unexpected<NodeError> grant_error(const trust::TrustError& error) {
    return p2p::fail(NodeError::Kind::grant, error.to_string());
}

std::uint64_t secs(std::int64_t ms) { return ms < 0 ? 0 : static_cast<std::uint64_t>(ms / 1000); }

std::string resource_label(const trust::Resource& resource,
                           std::span<const std::pair<trust::Resource, std::string>> labels) {
    for (const auto& [r, label] : labels) {
        const bool same = (std::holds_alternative<res::FutureSessions>(r) && std::holds_alternative<res::FutureSessions>(resource)) ||
                          r == resource;
        if (same) return label;
    }
    return std::visit(overloaded{
                          [](const res::Session& s) { return "Session " + s.id; },
                          [](const res::Prompt& p) {
                              return "Prompt " + std::to_string(p.turn.value) + " of session " + p.session;
                          },
                          [](const res::Project& p) { return "Project " + p.id; },
                          [](const res::FutureSessions&) { return std::string("Every future session"); },
                          [](const res::Ssh& s) { return "SSH " + s.host; },
                          [](const res::Surface& s) { return s.uri; },
                          [](const res::Sandbox&) { return std::string(p2p::SANDBOX_LABEL); },
                          [](const res::Folder& f) { return "Folder " + f.path; },
                      },
                      resource);
}

std::pair<std::string, bool> availability(const p2p::Availability& value) {
    switch (value.state) {
    case p2p::Availability::State::off: return {"off: " + value.detail, false};
    case p2p::Availability::State::active: return {value.detail, true};
    case p2p::Availability::State::failed: return {"failed: " + value.detail, false};
    }
    return {value.detail, false};
}

bool contains(const std::vector<trust::Action>& actions, trust::Action action) {
    return std::ranges::find(actions, action) != actions.end();
}

bool online(const std::vector<p2p::PeerInfo>& peers, const p2p::NodeId& key) {
    return std::ranges::any_of(peers, [&](const p2p::PeerInfo& p) { return p.key == key && p.online; });
}

p2p::Result<Outcome> run(p2p::Node& node, panel::Intent intent) {
    namespace i = panel::intent;
    return std::visit(
        overloaded{
            [&](i::ValidateKey& e) -> p2p::Result<Outcome> {
                auto parsed = p2p::Node::parse_key(e.key);
                const bool valid = parsed && *parsed != node.key();
                return outcome::KeyChecked{std::move(e.key), valid};
            },
            [&](i::AddPeer& e) -> p2p::Result<Outcome> {
                auto address = e.address ? std::optional<std::string_view>(*e.address) : std::nullopt;
                if (auto r = node.add_peer(e.key, address); !r) return std::unexpected(r.error());
                return outcome::Done{};
            },
            [&](i::AddDiscovered& e) -> p2p::Result<Outcome> {
                if (auto r = node.add_nearby(e.key); !r) return std::unexpected(r.error());
                return outcome::Done{};
            },
            [&](i::CopyKey& e) -> p2p::Result<Outcome> { return outcome::Copy{std::move(e.key)}; },
            [&](i::RemovePeer& e) -> p2p::Result<Outcome> {
                if (auto r = node.remove_peer(e.key); !r) return std::unexpected(r.error());
                return outcome::Done{};
            },
            [&](i::RenamePeer& e) -> p2p::Result<Outcome> {
                if (auto r = node.rename_peer(e.key, e.name); !r) return std::unexpected(r.error());
                return outcome::Done{};
            },
            [&](i::CreateGrant& e) -> p2p::Result<Outcome> {
                auto request = grant_request(e.input, runtime::time::now_ms());
                if (!request) return std::unexpected(request.error());
                auto issued = node.issue_grant(e.input.peer_key, std::move(request->first), request->second, e.input.reshare);
                if (!issued) return std::unexpected(issued.error());
                return outcome::Notice{"Grant created"};
            },
            [&](i::RevokeGrant& e) -> p2p::Result<Outcome> {
                if (auto r = node.revoke_grant(trust::GrantId{e.id}); !r) return std::unexpected(r.error());
                return outcome::Notice{"Grant revoked"};
            },
            [&](i::RenewGrant& e) -> p2p::Result<Outcome> {
                const trust::GrantId id{e.id};
                auto issued = node.grant_store()->issued();
                if (!issued) return grant_error(issued.error());
                auto record = std::ranges::find_if(*issued, [&](const trust::IssuedRecord& g) { return g.id == id; });
                if (record == issued->end()) return refused("unknown grant");
                // Renewing gives the grant its original length again from now.
                std::optional<std::int64_t> until;
                if (record->until_ms)
                    until = runtime::time::now_ms() + std::max<std::int64_t>(*record->until_ms - record->created_ms, 60'000);
                if (auto r = node.renew_grant(id, until); !r) return std::unexpected(r.error());
                return outcome::Notice{"Grant renewed"};
            },
            [&](i::Refresh&) -> p2p::Result<Outcome> {
                node.refresh_presence();
                return outcome::Done{};
            },
            [&](i::ScanTailscale&) -> p2p::Result<Outcome> {
                auto found = node.scan_tailscale();
                if (!found) return std::unexpected(found.error());
                return outcome::Notice{"Tailnet scan found " + std::to_string(*found) + " node" + (*found == 1 ? "" : "s")};
            },
            [&](i::OpenRemote& e) -> p2p::Result<Outcome> {
                auto view = node.open_remote(e.peer, e.session);
                if (!view) return std::unexpected(view.error());
                return outcome::Opened{std::shared_ptr<p2p::RemoteView>(std::move(*view))};
            },
            [&](i::ForkRemote& e) -> p2p::Result<Outcome> {
                auto id = node.fork_remote(e.peer, e.session);
                if (!id) return std::unexpected(id.error());
                return outcome::Forked{std::move(*id)};
            },
            [&](i::BrowseFolder& e) -> p2p::Result<Outcome> {
                auto entries = node.list_folder(e.peer, e.folder, "");
                if (!entries) return std::unexpected(entries.error());
                const bool can_write = std::ranges::any_of(node.shared_folders(), [&](const p2p::SharedFolder& f) {
                    return f.peer.to_string() == e.peer && f.path == e.folder && f.can_write;
                });
                return outcome::Folder{std::move(e.peer), std::move(e.folder), std::move(*entries), can_write};
            },
        },
        intent);
}

} // namespace

std::string resource_id(const trust::Resource& resource) {
    return std::visit(overloaded{
                          [](const res::Session& s) { return "session:" + s.id; },
                          [](const res::Prompt& p) { return "prompt:" + std::to_string(p.turn.value) + ":" + p.session; },
                          [](const res::Project& p) { return "project:" + p.id; },
                          [](const res::FutureSessions&) { return std::string("future"); },
                          [](const res::Ssh& s) { return "ssh:" + s.host; },
                          [](const res::Surface& s) { return "surface:" + s.uri; },
                          [](const res::Sandbox&) { return std::string("sandbox"); },
                          [](const res::Folder& f) { return "folder:" + f.path; },
                      },
                      resource);
}

std::optional<trust::Resource> parse_resource(std::string_view id, std::int64_t now_ms) {
    if (id == "future") return res::FutureSessions{now_ms};
    if (id == "sandbox") return res::Sandbox{};
    const auto colon = id.find(':');
    if (colon == std::string_view::npos) return std::nullopt;
    const auto kind = id.substr(0, colon);
    const std::string rest(id.substr(colon + 1));
    if (kind == "session") return res::Session{rest};
    if (kind == "prompt") {
        const auto split = rest.find(':');
        if (split == std::string::npos) return std::nullopt;
        std::uint64_t turn = 0;
        const char* first = rest.data();
        const char* last = rest.data() + split;
        auto [end, ec] = std::from_chars(first, last, turn);
        if (ec != std::errc{} || end != last || first == last) return std::nullopt;
        return res::Prompt{rest.substr(split + 1), core::NodeId{turn}};
    }
    if (kind == "project") return res::Project{rest};
    if (kind == "ssh") return res::Ssh{rest};
    if (kind == "surface") return res::Surface{rest};
    if (kind == "folder") return res::Folder{rest};
    return std::nullopt;
}

panel::GrantScope scope_kind(const trust::Resource& resource) {
    using S = panel::GrantScope;
    return std::visit(overloaded{
                          [](const res::Session&) { return S::Session; },
                          [](const res::Prompt&) { return S::Prompt; },
                          [](const res::Project&) { return S::Project; },
                          [](const res::FutureSessions&) { return S::FutureSessions; },
                          [](const res::Ssh&) { return S::SshHost; },
                          [](const res::Surface&) { return S::Surface; },
                          [](const res::Sandbox&) { return S::Sandbox; },
                          [](const res::Folder&) { return S::Folder; },
                      },
                      resource);
}

p2p::Result<std::vector<trust::Action>> actions(panel::Permission permission, panel::GrantScope scope) {
    using P = panel::Permission;
    using S = panel::GrantScope;
    using A = trust::Action;
    if (scope == S::Sandbox) {
        if (permission == P::Drive) return std::vector{A::exec};
        return refused("a sandbox grant only runs tasks (drive)");
    }
    if ((scope == S::SshHost || scope == S::Folder) && permission == P::View) return std::vector{A::read};
    if (scope == S::Folder && permission == P::Drive) return std::vector{A::read, A::write};
    if (scope == S::SshHost && permission == P::Drive) return std::vector{A::read, A::write, A::exec};
    if ((scope == S::SshHost || scope == S::Surface || scope == S::Folder) && (permission == P::Suggest || permission == P::Fork))
        return refused(std::string(core::as_str(permission)) + " does not apply to this resource");
    switch (permission) {
    case P::View: return std::vector{A::view};
    case P::Suggest: return std::vector{A::suggest};
    case P::Drive: return std::vector{A::drive};
    case P::Fork: return std::vector{A::fork};
    }
    return std::vector{A::view};
}

panel::Permission permission(const trust::ActionSet& actions) {
    using A = trust::Action;
    if (actions.contains(A::drive) || actions.contains(A::exec) || actions.contains(A::write)) return panel::Permission::Drive;
    if (actions.contains(A::suggest)) return panel::Permission::Suggest;
    if (actions.contains(A::fork)) return panel::Permission::Fork;
    return panel::Permission::View;
}

p2p::Result<std::pair<std::vector<trust::Scope>, std::optional<std::int64_t>>> grant_request(const panel::GrantInput& input,
                                                                                            std::int64_t now_ms) {
    auto parsed = parse_resource(input.resource.id, now_ms);
    if (!parsed) return refused("unknown resource " + input.resource.id);
    trust::Resource resource = std::move(*parsed);
    if (auto* folder = std::get_if<res::Folder>(&resource)) {
        auto real = local_folder(folder->path);
        if (!real) return std::unexpected(real.error());
        folder->path = std::move(*real);
    }
    auto granted = actions(input.permission, scope_kind(resource));
    if (!granted) return std::unexpected(granted.error());
    std::vector<trust::Scope> scopes;
    auto scope = trust::Scope::make(resource, *granted);
    if (!scope) return grant_error(scope.error());
    scopes.push_back(std::move(*scope));
    if (input.resource.future_sessions && std::holds_alternative<res::Prompt>(resource)) {
        auto future = trust::Scope::make(res::FutureSessions{now_ms}, *granted);
        if (!future) return grant_error(future.error());
        scopes.push_back(std::move(*future));
    }
    // `i64::try_from(ttl).unwrap_or(i64::MAX / 2).saturating_mul(1000)`, then a saturating add.
    constexpr std::int64_t max = std::numeric_limits<std::int64_t>::max();
    const std::int64_t ttl = input.ttl_seconds > static_cast<std::uint64_t>(max) ? max / 2 : static_cast<std::int64_t>(input.ttl_seconds);
    const std::int64_t ttl_ms = ttl > max / 1000 ? max : ttl * 1000;
    const std::int64_t until = now_ms > 0 && ttl_ms > max - now_ms ? max : now_ms + ttl_ms;
    return std::pair{std::move(scopes), std::optional<std::int64_t>(until)};
}

p2p::Result<std::string> local_folder(std::string_view typed) {
    // Checked on this node, so not a peer's refusal.
    auto error = [](std::string text) { return p2p::fail(NodeError::Kind::unavailable, "Can't share " + text); };
    const auto first = typed.find_first_not_of(" \t\r\n");
    const auto last = typed.find_last_not_of(" \t\r\n");
    const std::string path(first == std::string_view::npos ? std::string_view{} : typed.substr(first, last - first + 1));
    if (!std::filesystem::path(path).is_absolute()) return error("\"" + path + "\": give the folder's full path");
    auto real = runtime::fs::canonicalize(path);
    if (!real) return error(path + ": " + real.error().message());
    std::error_code ec;
    if (!std::filesystem::is_directory(*real, ec)) return error(path + ": not a folder");
    return real->string();
}

std::optional<panel::Grant> grant(const p2p::GrantInfo& info, std::span<const std::pair<trust::Resource, std::string>> labels) {
    if (info.scopes.empty()) return std::nullopt;
    auto primary_it = std::ranges::find_if(info.scopes, [](const trust::Scope& s) { return !std::holds_alternative<res::FutureSessions>(s.resource); });
    const trust::Scope& primary = primary_it != info.scopes.end() ? *primary_it : info.scopes.front();
    const bool future_sessions =
        std::holds_alternative<res::Prompt>(primary.resource) &&
        std::ranges::any_of(info.scopes, [](const trust::Scope& s) { return std::holds_alternative<res::FutureSessions>(s.resource); });
    panel::Grant out;
    out.id = info.id.as_str();
    out.peer_key = info.peer.to_string();
    out.direction = info.direction == p2p::GrantDirection::given ? panel::GrantDirection::Given : panel::GrantDirection::Received;
    out.resource = panel::GrantResource{resource_id(primary.resource), resource_label(primary.resource, labels),
                                        scope_kind(primary.resource), future_sessions};
    out.permission = permission(primary.actions);
    if (auto end = info.until_ms ? info.until_ms : info.expires_ms) out.expires_at = secs(*end);
    if (info.renews_at_ms) out.renews_at = secs(*info.renews_at_ms);
    out.reshare = info.reshare;
    out.ended = info.ended;
    return out;
}

panel::Data data(const p2p::Node& node, std::int64_t now_ms) {
    const auto status = node.status();
    std::string endpoint;
    bool endpoint_ok = false;
    std::visit(overloaded{
                   [&](const p2p::endpoint::Stopped&) {},
                   [&](const p2p::endpoint::Starting&) { endpoint = "starting"; },
                   [&](const p2p::endpoint::Listening& l) {
                       std::string udp;
                       for (const auto& addr : l.udp) udp += (udp.empty() ? "" : ", ") + addr;
                       endpoint = "UDP " + udp + " · TCP " + l.tcp;
                       endpoint_ok = true;
                   },
                   [&](const p2p::endpoint::Failed& f) { endpoint = "failed: " + f.error; },
               },
               status.endpoint);
    auto [lan, lan_ok] = availability(status.mdns);
    auto [tailscale, tailscale_ok] = availability(status.tailscale);
    panel::Data out;
    out.node = panel::NodeCard{
        .key = status.key.to_string(),
        .name = status.name,
        .key_source = std::visit(overloaded{
                                     [](const trust::key_source::Keychain&) { return std::string("OS keychain"); },
                                     [](const trust::key_source::File& f) {
                                         return f.path.string() + " (keychain unavailable: " + f.keychain_error + ")";
                                     },
                                     [](const trust::key_source::Memory& m) { return "in memory only (" + m.reason + ")"; },
                                 },
                                 status.key_source),
        .endpoint = std::move(endpoint),
        .endpoint_ok = endpoint_ok,
        .lan = std::move(lan),
        .lan_ok = lan_ok,
        .tailscale = std::move(tailscale),
        .tailscale_ok = tailscale_ok,
        .tailscale_available = node.config().tailscale.has_value() && endpoint_ok,
    };

    const auto peers_info = node.peers();
    for (const auto& peer : peers_info) {
        panel::Peer p;
        p.key = peer.key.to_string();
        p.name = peer.name && !peer.name->empty() ? *peer.name : p2p::short_key(peer.key);
        switch (peer.source) {
        case p2p::DiscoverySource::mdns: p.discovery = panel::Discovery::Lan; break;
        case p2p::DiscoverySource::tailscale: p.discovery = panel::Discovery::Tailscale; break;
        case p2p::DiscoverySource::manual: p.discovery = panel::Discovery::AddedByKey; break;
        }
        p.presence = peer.online ? panel::Presence::Online : panel::Presence::Offline;
        if (peer.last_seen_ms) p.last_seen = secs(*peer.last_seen_ms);
        p.transport = panel::Transport::None;
        if (peer.transport) {
            switch (*peer.transport) {
            case p2p::TransportKind::quic: p.transport = panel::Transport::Direct; break;
            case p2p::TransportKind::relay: p.transport = panel::Transport::Relay; break;
            case p2p::TransportKind::tcp: p.transport = panel::Transport::TcpFallback; break;
            }
        }
        p.app_version = peer.app_version.value_or("");
        p.shared_protocol = peer.shared_protocol;
        out.peers.push_back(std::move(p));
    }
    for (const auto& near : node.nearby()) {
        panel::Peer p;
        p.key = near.key.to_string();
        p.name = near.name ? *near.name : p2p::short_key(near.key);
        switch (near.source) {
        case p2p::NearbySource::mdns: p.discovery = panel::Discovery::Lan; break;
        case p2p::NearbySource::tailscale: p.discovery = panel::Discovery::Tailscale; break;
        case p2p::NearbySource::incoming: p.discovery = panel::Discovery::Incoming; break;
        }
        p.presence = panel::Presence::Online;
        p.last_seen = secs(near.seen_ms);
        p.transport = panel::Transport::None;
        p.app_version = near.app_version.value_or("");
        out.nearby.push_back(std::move(p));
    }

    const auto labels = node.resources();
    const auto remote = node.remote_sessions();
    // Received grants name the peer's sessions: label them with the titles it listed.
    auto known = labels;
    for (const auto& s : remote)
        if (!s.title.empty()) known.emplace_back(res::Session{s.id}, s.title);
    for (const auto& info : node.grant_infos())
        if (auto g = grant(info, known)) out.grants.push_back(std::move(*g));
    for (const auto& [resource, label] : labels)
        out.resources.push_back(panel::ResourceChoice{resource_id(resource), label, scope_kind(resource)});
    for (const auto& session : remote) {
        std::string access;
        if (session.actions.empty()) {
            access = "some prompts";
        } else {
            for (auto a : session.actions) access += (access.empty() ? "" : ", ") + std::string(trust::as_str(a));
        }
        out.remote_sessions.push_back(panel::RemoteItem{
            .peer_key = session.peer.to_string(),
            .peer_name = node.peer_name(session.peer),
            .session = session.id,
            .title = session.title.empty() ? "Session " + session.id : session.title,
            .harness = session.harness,
            .access = std::move(access),
            .can_drive = contains(session.actions, trust::Action::drive),
            .can_fork = contains(session.actions, trust::Action::fork),
            .online = online(peers_info, session.peer),
        });
    }
    for (const auto& folder : node.shared_folders())
        out.shared_folders.push_back(panel::SharedFolderItem{
            .peer_key = folder.peer.to_string(),
            .peer_name = node.peer_name(folder.peer),
            .path = folder.path,
            .can_write = folder.can_write,
            .online = online(peers_info, folder.peer),
        });
    out.local_version = status.app_version;
    out.now = secs(now_ms);
    return out;
}

std::expected<Outcome, std::string> apply(p2p::Node& node, panel::Intent intent) {
    auto result = run(node, std::move(intent));
    if (!result) return std::unexpected(result.error().to_string());
    return std::move(*result);
}

} // namespace loa::app::areas::peers
