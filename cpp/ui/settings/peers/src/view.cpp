// Qt view of the peers panel. Port of `crates/loa-ui-settings/src/peers/view.rs`.

#include "loa/ui/settings/peers/view.hpp"

#include <algorithm>
#include <cctype>
#include <format>

#include <QCheckBox>
#include <QComboBox>
#include <QFrame>
#include <QLabel>
#include <QLineEdit>
#include <QPushButton>
#include <QScrollArea>
#include <QVBoxLayout>

#include "settings_widgets.hpp"
#include "strings.hpp"

namespace loa::ui::settings::peers {

using namespace sw;
using loa::core::as_str;
namespace in = vp::intent;

namespace {

// Resource choices shown at once; the filter narrows longer lists.
constexpr std::size_t RESOURCE_ROWS = 8;

Tone online(bool on) { return on ? Tone::Ok : Tone::Faint; }
QLabel* caption(std::string_view s) { return kw::label(s, TextRole::Caption, Tone::Muted); }
QLabel* dot(Tone tone) {
    auto* d = kw::label("\u25cf", TextRole::Caption, tone);
    d->setAccessibleName(QStringLiteral("status"));
    return d;
}
QFrame* divider() {
    auto* f = new QFrame;
    f->setFrameShape(QFrame::HLine);
    f->setAccessibleName(QStringLiteral("divider"));
    return f;
}

// A list entry: a status light and title over its other lines.
QWidget* item(Tone tone, std::string_view title, QWidget* lines) {
    return vcol({hrow({dot(tone), kw::label(title, TextRole::Body, Tone::Normal)}), lines});
}

} // namespace

PeersView::PeersView(QWidget* parent) : QWidget(parent) {
    auto* l = new QVBoxLayout(this);
    l->setContentsMargins(0, 0, 0, 0);
    setAccessibleName(QStringLiteral("peers settings"));
}

void PeersView::render(const State& state) {
    namespace l = local;
    const auto& d = state.data;
    auto post = [this](Message m) { emit message(m); };
    auto send = [post](Local m) -> Callback { return [post, m] { post(Message::local(m)); }; };
    auto ask = [post](Intent e) -> Callback { return [post, e] { post(Message::event(e)); }; };
    auto confirm_row = [&](std::string_view question, std::string_view yes) {
        return hrow({caption(question), action(yes, ButtonKind::danger(), send(l::Confirm{})), action("Cancel", ButtonKind::ghost(), send(l::Cancel{}))},
                    spi(SpaceRole::Xs));
    };

    // This node.
    const auto& node = d.node;
    auto status = [&](std::string_view name, std::string_view value, Tone tone) {
        auto* n = caption(name);
        n->setFixedWidth(px(theme::width(LayoutRole::LabelColumn) / 2.0f));
        return hrow({dot(tone), n, kw::label(value, TextRole::Caption, Tone::Normal)});
    };
    const Tone endpoint_tone = node.endpoint_ok ? Tone::Ok : node.endpoint.empty() ? Tone::Faint : Tone::Danger;
    auto* actions = hrow({action("Copy key", ButtonKind::ghost(), ask(in::CopyKey{node.key})), action("Refresh", ButtonKind::ghost(), ask(in::Refresh{})),
                          node.tailscale_available ? action("Scan tailnet", ButtonKind::ghost(), ask(in::ScanTailscale{})) : nullptr},
                         spi(SpaceRole::Xs));
    auto* node_view = section("This node", vcol({
        hrow({kw::label(node.name, TextRole::Body, Tone::Normal), caption(std::string_view(node.key).substr(0, 12))}),
        status("Key", node.key_source, Tone::Ok),
        status("Endpoint", node.endpoint.empty() ? std::string_view("not started") : std::string_view(node.endpoint), endpoint_tone),
        status("LAN", node.lan, online(node.lan_ok)),
        status("Tailscale", node.tailscale, online(node.tailscale_ok)),
        actions,
    }));

    auto peer_row = [&](const Peer& peer, bool nearby) -> QWidget* {
        auto* acts = hrow({action("Copy full key", ButtonKind::ghost(), ask(in::CopyKey{peer.key}))}, spi(SpaceRole::Xs));
        if (nearby) {
            add(acts, action("Add", ButtonKind::ghost(), ask(in::AddDiscovered{peer.key})));
        } else {
            add(acts, action("Grants", ButtonKind::ghost(), send(l::SelectPeer{peer.key})));
            add(acts, action("Rename", ButtonKind::ghost(), send(l::BeginRename{peer.key})));
            add(acts, action("Remove", ButtonKind::danger(), send(l::Ask{confirm::RemovePeer{peer.key}})));
        }
        const auto seen = std::format("{} \u00b7 {} \u00b7 {} \u00b7 last seen {} \u00b7 {} ({})", as_str(peer.discovery), as_str(peer.presence),
                                      as_str(peer.transport), peer.last_seen_text(d.now), peer.app_version,
                                      as_str(peer.version_relation(d.local_version)));
        const char* protocol = !peer.shared_protocol ? "Not contacted yet" : *peer.shared_protocol ? "Shared protocol available" : "No shared protocol";
        auto* lines = vcol({caption(seen), caption(protocol), acts});
        if (state.confirm == Confirm{confirm::RemovePeer{peer.key}}) add(lines, confirm_row("Remove this peer?", "Confirm remove"));
        if (state.rename && state.rename->first == peer.key) {
            auto* f = field("Peer name", state.rename->second, [post](std::string n) { post(Message::local(l::RenameText{std::move(n)})); }, "loa-peer-rename");
            QObject::connect(f, &QLineEdit::returnPressed, f, send(l::SaveRename{}));
            add(lines, hrow({f, action("Save", ButtonKind::ghost(), send(l::SaveRename{})), action("Cancel", ButtonKind::ghost(), send(l::Cancel{}))},
                            spi(SpaceRole::Xs)));
        }
        return item(online(peer.presence == vp::Presence::Online), std::format("{}  {}", peer.name, peer.short_key()), lines);
    };

    // Peers, nearby and adding by key.
    auto* sort = new QComboBox;
    sort->setAccessibleName(QStringLiteral("Sort peers"));
    for (auto s : PEER_SORTS) sort->addItem(qs(as_str(s)));
    sort->setCurrentIndex(static_cast<int>(state.sort));
    QObject::connect(sort, &QComboBox::activated, sort, [post](int i) { post(Message::local(l::Sort{PEER_SORTS.at(static_cast<std::size_t>(i))})); });
    auto* filter = field("Filter peers", state.filter, [post](std::string f) { post(Message::local(l::Filter{std::move(f)})); }, "loa-peers-filter");
    auto* peers = vcol({hrow({filter, sort})}, spi(SpaceRole::S));
    for (const auto* p : state.visible_peers()) add(peers, peer_row(*p, false));
    const auto near = state.visible_nearby();
    auto* nearby = vcol({}, spi(SpaceRole::S));
    for (const auto* p : near) add(nearby, peer_row(*p, true));
    if (near.empty()) add(nearby, caption("No other nodes seen on the LAN or tailnet yet"));
    const bool invalid = !str::trim(state.add_key).empty() && state.key_validation && !state.key_validation->second;
    auto* add_by_key = vcol({field("Public key", state.add_key, [post](std::string k) { post(Message::local(l::Key{std::move(k)})); }, "loa-peers-key"),
                             field("Address (optional)", state.add_address, [post](std::string a) { post(Message::local(l::Address{std::move(a)})); },
                                   "loa-peers-address"),
                             invalid ? kw::label("Invalid public key", TextRole::Caption, Tone::Danger) : nullptr,
                             action("Add peer", ButtonKind::primary(), state.key_valid() ? send(l::AddPeer{}) : Callback{})},
                            spi(SpaceRole::S));

    // Peers' sessions and folders.
    auto* shared = vcol({}, spi(SpaceRole::S));
    for (const auto& r : d.remote_sessions) {
        auto at = [&](Intent e) { return r.online ? ask(std::move(e)) : Callback{}; };
        auto* row = hrow({action(r.can_drive ? "Open and drive" : "Open", ButtonKind::ghost(), at(in::OpenRemote{r.peer_key, r.session})),
                          r.can_fork ? action("Fork to this node", ButtonKind::ghost(), at(in::ForkRemote{r.peer_key, r.session})) : nullptr},
                         spi(SpaceRole::Xs));
        add(shared, item(online(r.online), r.title, vcol({caption(std::format("{} \u00b7 {} \u00b7 {}", r.peer_name, r.harness, r.access)), row})));
    }
    if (d.remote_sessions.empty()) add(shared, caption("No peer has shared a session with this node"));
    auto* folders = vcol({}, spi(SpaceRole::S));
    for (const auto& f : d.shared_folders) {
        auto* browse = action("Browse", ButtonKind::ghost(), f.online ? ask(in::BrowseFolder{f.peer_key, f.path}) : Callback{});
        add(folders, item(online(f.online), f.path,
                          vcol({caption(std::format("{} \u00b7 {}", f.peer_name, f.can_write ? "read, write" : "read")), browse})));
    }
    if (d.shared_folders.empty()) add(folders, caption("No peer has shared a folder with this node"));

    // The selected peer's grants and the grant form.
    QWidget* grants_view = nullptr;
    if (!state.selected_peer) {
        grants_view = caption("Select a peer to manage grants");
    } else {
        const auto& key = *state.selected_peer;
        const auto pit = std::ranges::find(d.peers, key, &Peer::key);
        const std::string name = pit == d.peers.end() ? std::string("this peer") : pit->name;
        auto* grants = vcol({});
        for (const auto& g : d.grants) {
            if (g.peer_key != key) continue;
            const bool given = g.direction == GrantDirection::Given;
            auto* line = hrow({caption(std::format("{}: {} \u00b7 {} \u00b7 {}", as_str(g.direction), grant_summary(g), g.expiry_text(d.now),
                                                   g.reshare ? "re-share allowed" : "no re-share")),
                               given ? action("Renew", ButtonKind::ghost(), ask(in::RenewGrant{g.id})) : nullptr,
                               given && !g.ended ? action("Revoke", ButtonKind::danger(), send(l::Ask{confirm::RevokeGrant{g.id}})) : nullptr});
            add(grants, line);
            if (state.confirm == Confirm{confirm::RevokeGrant{g.id}}) add(grants, confirm_row("Revoke this grant?", "Confirm revoke"));
        }

        const auto& form = state.grant_form;
        std::string term = std::string(str::trim(state.resource_filter));
        std::ranges::transform(term, term.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
        std::vector<const ResourceChoice*> matching;
        for (const auto& r : d.resources) {
            std::string lab = r.label;
            std::ranges::transform(lab, lab.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
            if (lab.find(term) != std::string::npos) matching.push_back(&r);
        }
        auto choice = [&](const ResourceChoice& r) -> QWidget* {
            return kw::menu_row(std::nullopt, r.label, form.resource && form.resource->id == r.id, send(l::SelectResource{r.id}));
        };
        auto* col = vcol({caption("Resource")}, spi(SpaceRole::S));
        if (d.resources.size() > RESOURCE_ROWS)
            add(col, field("Filter sessions, prompts, hosts", state.resource_filter,
                           [post](std::string f) { post(Message::local(l::ResourceFilter{std::move(f)})); }, "loa-grant-resource-filter"));
        const auto shown = std::min(matching.size(), RESOURCE_ROWS);
        if (form.resource && !std::any_of(matching.begin(), matching.begin() + static_cast<std::ptrdiff_t>(shown),
                                          [&](const ResourceChoice* r) { return r->id == form.resource->id; }))
            add(col, choice(*form.resource));
        for (std::size_t i = 0; i < shown; ++i) add(col, choice(*matching[i]));
        if (matching.size() > RESOURCE_ROWS) add(col, caption(std::format("{} more; filter to find them", matching.size() - RESOURCE_ROWS)));
        auto* folder = field("Folder on this node, e.g. /home/me/shared", form.folder_path,
                             [post](std::string p) { post(Message::local(l::FolderPath{std::move(p)})); }, "loa-grant-folder");
        QObject::connect(folder, &QLineEdit::returnPressed, folder, send(l::UseFolder{}));
        add(col, hrow({folder, action("Use folder", ButtonKind::ghost(), send(l::UseFolder{}))}));
        if (form.resource) {
            const auto scope = form.resource->scope;
            if (scope == GrantScope::Prompt)
                add(col, check(form.future_sessions, "Include every future session", [post](bool on) { post(Message::local(l::FutureSessions{on})); }));
            add(col, caption("Permission"));
            for (auto p : {Permission::View, Permission::Suggest, Permission::Drive, Permission::Fork})
                if (vp::allowed_for(p, scope)) add(col, kw::menu_row(std::nullopt, as_str(p), form.permission == p, send(l::SetPermission{p})));
            if (form.is_ssh() && !form.ssh_confirmed) {
                add(col, caption("SSH access is off until explicitly confirmed."));
                add(col, action("Enable SSH grant", ButtonKind::ghost(), send(l::Ask{confirm::Ssh{}})));
                if (state.confirm == Confirm{confirm::Ssh{}})
                    add(col, confirm_row("Allow this peer to request SSH access through this node?", "Confirm SSH access"));
            }
        }
        auto* ttl = field("hours", form.ttl_hours, [post](std::string t) { post(Message::local(l::Ttl{std::move(t)})); }, "loa-grant-ttl");
        ttl->setFixedWidth(px(theme::width(LayoutRole::LabelColumn) / 2.0f));
        add(col, hrow({caption("Lasts (hours)"), ttl}));
        add(col, check(form.reshare, "Allow re-share", [post](bool on) { post(Message::local(l::Reshare{on})); }));
        add(col, action("Create grant", ButtonKind::primary(), form.build(key) ? send(l::SubmitGrant{}) : Callback{}));
        grants_view = vcol({section(std::format("Grants with {}", name), grants), section("Create grant", col)}, spi(SpaceRole::M));
    }

    // Grants this node holds.
    auto* received = vcol({});
    bool any_received = false;
    for (const auto& g : d.grants) {
        if (g.direction != GrantDirection::Received) continue;
        any_received = true;
        const auto pit = std::ranges::find(d.peers, g.peer_key, &Peer::key);
        const std::string from = pit == d.peers.end() ? g.peer_key.substr(0, 12) : pit->name;
        add(received, caption(std::format("From {}: {} \u00b7 {}", from, grant_summary(g), g.expiry_text(d.now))));
    }
    if (!any_received) add(received, caption("No peer has granted this node anything"));

    auto* content = vcol({node_view, divider(), section("Peers", peers), section("Discovered nearby", nearby), section("Add by public key", add_by_key),
                          divider(), section("Shared with you", shared), section("Shared folders", folders), divider(), grants_view, divider(),
                          section("Received grants", received)},
                         spi(SpaceRole::M));
    stretch(content);
    content_ = swap_content(*this, content_, sw::scroll(content, true));
}

} // namespace loa::ui::settings::peers
