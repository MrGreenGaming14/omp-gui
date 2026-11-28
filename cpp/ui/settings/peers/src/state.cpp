// Port of `crates/loa-ui-settings/src/peers/{mod,state,grant_form}.rs`.

#include "loa/ui/settings/peers/state.hpp"

#include <algorithm>
#include <charconv>
#include <format>
#include <limits>

#include "strings.hpp"

namespace loa::ui::settings::peers {

namespace in = vp::intent;
using loa::core::as_str;

namespace {

std::string lower(std::string_view s) {
    std::string out(s);
    for (auto& c : out)
        if (c >= 'A' && c <= 'Z') c = static_cast<char>(c - 'A' + 'a');
    return out;
}

bool contains(std::string_view hay, std::string_view needle) { return hay.find(needle) != std::string_view::npos; }

// The permission a newly picked resource starts with.
Permission default_permission(std::optional<GrantScope> scope) {
    if (!scope) return Permission::Fork;
    switch (*scope) {
    case GrantScope::Surface:
    case GrantScope::SshHost:
    case GrantScope::Folder: return Permission::View;
    case GrantScope::Sandbox: return Permission::Drive;
    default: return Permission::Fork;
    }
}

// Rust `str::parse::<u64>`: an optional `+`, then decimal digits only.
std::optional<std::uint64_t> parse_u64(std::string_view s) {
    if (!s.empty() && s.front() == '+') s.remove_prefix(1);
    if (s.empty()) return std::nullopt;
    std::uint64_t v = 0;
    const auto [end, ec] = std::from_chars(s.data(), s.data() + s.size(), v);
    if (ec != std::errc{} || end != s.data() + s.size()) return std::nullopt;
    return v;
}

} // namespace

std::vector<const Peer*> visible(const std::vector<Peer>& peers, std::string_view filter, PeerSort sort) {
    const auto term = lower(str::trim(filter));
    std::vector<const Peer*> out;
    for (const auto& p : peers)
        if (contains(lower(p.name), term) || contains(lower(p.key), term) || contains(lower(as_str(p.discovery)), term)) out.push_back(&p);
    std::ranges::stable_sort(out, [sort](const Peer* a, const Peer* b) {
        auto order = std::strong_ordering::equal;
        switch (sort) {
        case PeerSort::Name: break;
        case PeerSort::OnlineFirst:
            order = (a->presence != vp::Presence::Online) <=> (b->presence != vp::Presence::Online);
            break;
        case PeerSort::RecentlySeen: order = b->last_seen <=> a->last_seen; break;
        case PeerSort::Version: order = b->version() <=> a->version(); break;
        }
        if (order == 0) order = lower(a->name) <=> lower(b->name);
        if (order == 0) order = a->key <=> b->key;
        return order < 0;
    });
    return out;
}

std::string grant_summary(const Grant& grant) {
    const auto& r = grant.resource;
    std::string label;
    if (r.scope == GrantScope::FutureSessions) label = "Every future session";
    else if (r.scope == GrantScope::Prompt && r.future_sessions) label = r.label + " and every session from now on";
    else label = r.label;
    return std::format("{}, {}", label, as_str(grant.permission));
}

std::string_view to_string(GrantFormError e) noexcept {
    switch (e) {
    case GrantFormError::MissingResource: return "MissingResource";
    case GrantFormError::InvalidTtl: return "InvalidTtl";
    case GrantFormError::InvalidPermission: return "InvalidPermission";
    case GrantFormError::SshNotConfirmed: return "SshNotConfirmed";
    }
    return {};
}

void GrantForm::select_resource(std::optional<ResourceChoice> next) {
    if (resource != next) {
        ssh_confirmed = false;
        future_sessions = false;
        permission = default_permission(next ? std::optional{next->scope} : std::nullopt);
    }
    resource = std::move(next);
}

std::optional<ResourceChoice> GrantForm::folder_choice() const {
    const auto path = str::trim(folder_path);
    if (path.empty()) return std::nullopt;
    return ResourceChoice{std::format("folder:{}", path), std::format("Folder {}", path), GrantScope::Folder};
}

bool GrantForm::is_ssh() const noexcept { return resource && resource->scope == GrantScope::SshHost; }

std::expected<GrantInput, GrantFormError> GrantForm::build(std::string_view peer_key) const {
    if (!resource) return std::unexpected(GrantFormError::MissingResource);
    const auto& r = *resource;
    if (!vp::allowed_for(permission, r.scope)) return std::unexpected(GrantFormError::InvalidPermission);
    if (is_ssh() && !ssh_confirmed) return std::unexpected(GrantFormError::SshNotConfirmed);
    const auto hours = parse_u64(str::trim(ttl_hours));
    if (!hours || *hours == 0 || *hours > std::numeric_limits<std::uint64_t>::max() / 3600)
        return std::unexpected(GrantFormError::InvalidTtl);
    GrantResource res{r.id, r.label, r.scope, r.scope == GrantScope::Prompt && future_sessions};
    return GrantInput{std::string(peer_key), std::move(res), permission, *hours * 3600, reshare};
}

void State::set_data(Data next) {
    bool backed = true;
    if (confirm) {
        if (const auto* c = std::get_if<confirm::RevokeGrant>(&*confirm))
            backed = std::ranges::any_of(next.grants, [&](const Grant& g) { return g.id == c->id && g.direction == GrantDirection::Given; });
        else if (const auto* c = std::get_if<confirm::RemovePeer>(&*confirm))
            backed = std::ranges::any_of(next.peers, [&](const Peer& p) { return p.key == c->key; });
    }
    if (!backed) confirm.reset();
    if (selected_peer && !std::ranges::any_of(next.peers, [&](const Peer& p) { return p.key == *selected_peer; })) {
        selected_peer.reset();
        grant_form = GrantForm{};
    }
    // A typed folder isn't in the app's list; the app checks it on issue.
    if (const auto& r = grant_form.resource; r && r->scope != GrantScope::Folder && std::ranges::find(next.resources, *r) == next.resources.end())
        grant_form = GrantForm{};
    data = std::move(next);
}

void State::set_key_validation(std::string_view key, bool valid) {
    if (str::trim(add_key) == key) key_validation = std::pair{std::string(key), valid};
}

std::vector<const Peer*> State::visible_peers() const { return visible(data.peers, filter, sort); }

std::vector<const Peer*> State::visible_nearby() const {
    auto out = visible(data.nearby, filter, sort);
    std::erase_if(out, [this](const Peer* n) { return is_peer(n->key); });
    return out;
}

bool State::key_valid() const { return key_validation && key_validation->second && key_validation->first == str::trim(add_key); }

bool State::is_peer(std::string_view key) const {
    return std::ranges::any_of(data.peers, [&](const Peer& p) { return p.key == key; });
}

bool State::given(std::string_view id) const {
    return std::ranges::any_of(data.grants, [&](const Grant& g) { return g.id == id && g.direction == GrantDirection::Given; });
}

void State::select_resource(std::optional<ResourceChoice> resource) {
    if (confirm && std::holds_alternative<confirm::Ssh>(*confirm)) confirm.reset();
    grant_form.select_resource(std::move(resource));
}

std::optional<Intent> State::reduce(Local m) {
    namespace l = local;
    const bool ssh = grant_form.is_ssh();
    return std::visit(
        [&]<class T>(T& v) -> std::optional<Intent> {
            if constexpr (std::is_same_v<T, l::Filter>) filter = std::move(v.value);
            else if constexpr (std::is_same_v<T, l::Sort>) sort = v.sort;
            else if constexpr (std::is_same_v<T, l::Key>) {
                add_key = std::move(v.value);
                key_validation.reset();
                return in::ValidateKey{str::trimmed(add_key)};
            } else if constexpr (std::is_same_v<T, l::Address>) add_address = std::move(v.value);
            else if constexpr (std::is_same_v<T, l::AddPeer>) {
                if (!key_valid()) return std::nullopt;
                const auto address = str::trim(add_address);
                Intent event = in::AddPeer{str::trimmed(add_key), address.empty() ? std::nullopt : std::optional{std::string(address)}};
                add_key.clear();
                add_address.clear();
                key_validation.reset();
                return event;
            } else if constexpr (std::is_same_v<T, l::Ask>) {
                const bool valid = std::visit(
                    [&]<class C>(const C& c) {
                        if constexpr (std::is_same_v<C, confirm::RemovePeer>) return is_peer(c.key);
                        else if constexpr (std::is_same_v<C, confirm::RevokeGrant>) return given(c.id);
                        else return ssh;
                    },
                    v.confirm);
                if (valid) confirm = std::move(v.confirm);
                else confirm.reset();
            } else if constexpr (std::is_same_v<T, l::Confirm>) {
                if (!confirm) return std::nullopt;
                auto c = std::move(*confirm);
                confirm.reset();
                if (auto* r = std::get_if<confirm::RemovePeer>(&c)) return in::RemovePeer{std::move(r->key)};
                if (auto* r = std::get_if<confirm::RevokeGrant>(&c)) return in::RevokeGrant{std::move(r->id)};
                grant_form.ssh_confirmed = ssh;
            } else if constexpr (std::is_same_v<T, l::Cancel>) {
                confirm.reset();
                rename.reset();
            } else if constexpr (std::is_same_v<T, l::BeginRename>) {
                const auto it = std::ranges::find(data.peers, v.key, &Peer::key);
                if (it == data.peers.end()) rename.reset();
                else rename = std::pair{std::move(v.key), it->name};
            } else if constexpr (std::is_same_v<T, l::RenameText>) {
                if (rename) rename->second = std::move(v.name);
            } else if constexpr (std::is_same_v<T, l::SaveRename>) {
                if (!rename) return std::nullopt;
                auto [key, name] = std::move(*rename);
                rename.reset();
                const auto trimmed = str::trim(name);
                if (trimmed.empty()) return std::nullopt;
                return in::RenamePeer{std::move(key), std::string(trimmed)};
            } else if constexpr (std::is_same_v<T, l::SelectPeer>) {
                if (is_peer(v.key)) selected_peer = std::move(v.key);
                else selected_peer.reset();
                grant_form = GrantForm{};
                resource_filter.clear();
                if (confirm && std::holds_alternative<confirm::Ssh>(*confirm)) confirm.reset();
            } else if constexpr (std::is_same_v<T, l::ResourceFilter>) resource_filter = std::move(v.value);
            else if constexpr (std::is_same_v<T, l::SelectResource>) {
                const auto it = std::ranges::find(data.resources, v.id, &ResourceChoice::id);
                select_resource(it == data.resources.end() ? std::nullopt : std::optional{*it});
            } else if constexpr (std::is_same_v<T, l::SetPermission>) grant_form.permission = v.permission;
            else if constexpr (std::is_same_v<T, l::Ttl>) grant_form.ttl_hours = std::move(v.value);
            else if constexpr (std::is_same_v<T, l::Reshare>) grant_form.reshare = v.on;
            else if constexpr (std::is_same_v<T, l::FutureSessions>) grant_form.future_sessions = v.on;
            else if constexpr (std::is_same_v<T, l::FolderPath>) grant_form.folder_path = std::move(v.path);
            else if constexpr (std::is_same_v<T, l::UseFolder>) {
                if (auto folder = grant_form.folder_choice()) select_resource(std::move(folder));
            } else if constexpr (std::is_same_v<T, l::SubmitGrant>) {
                if (!selected_peer || !is_peer(*selected_peer)) return std::nullopt;
                auto input = grant_form.build(*selected_peer);
                if (!input) return std::nullopt;
                return in::CreateGrant{std::move(*input)};
            }
            return std::nullopt;
        },
        m);
}

bool State::guard(const Intent& e) const {
    const auto& d = data;
    return std::visit(
        [&]<class T>(const T& v) -> bool {
            if constexpr (std::is_same_v<T, in::AddDiscovered>)
                return std::ranges::any_of(d.nearby, [&](const Peer& p) { return p.key == v.key; }) && !is_peer(v.key);
            else if constexpr (std::is_same_v<T, in::CopyKey>)
                return v.key == d.node.key || is_peer(v.key) || std::ranges::any_of(d.nearby, [&](const Peer& p) { return p.key == v.key; });
            else if constexpr (std::is_same_v<T, in::RenewGrant>) return given(v.id);
            else if constexpr (std::is_same_v<T, in::Refresh>) return true;
            else if constexpr (std::is_same_v<T, in::ScanTailscale>) return d.node.tailscale_available;
            else if constexpr (std::is_same_v<T, in::OpenRemote>)
                return std::ranges::any_of(d.remote_sessions, [&](const vp::RemoteItem& r) { return r.peer_key == v.peer && r.session == v.session; });
            else if constexpr (std::is_same_v<T, in::ForkRemote>)
                return std::ranges::any_of(d.remote_sessions,
                                           [&](const vp::RemoteItem& r) { return r.peer_key == v.peer && r.session == v.session && r.can_fork; });
            else if constexpr (std::is_same_v<T, in::BrowseFolder>)
                return std::ranges::any_of(d.shared_folders,
                                           [&](const vp::SharedFolderItem& f) { return f.peer_key == v.peer && f.path == v.folder && f.online; });
            else return false;
        },
        e);
}

} // namespace loa::ui::settings::peers
