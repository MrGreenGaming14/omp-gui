// The `peers` and `updates` smoke screens: the peers panel with a peer's grant form open, and
// the updates screen with a held update open, each over sample view models and its reducer.

#include <QAbstractButton>
#include <QLabel>
#include <QVBoxLayout>
#include <QWidget>

#include "loa/ui/app/smoke.hpp"
#include "loa/ui/settings/peers/view.hpp"
#include "loa/ui/settings/updates/view.hpp"

namespace loa::ui::app {

namespace {

namespace peers = settings::peers;
namespace updates = settings::updates;

// A screen state owned by the widget that renders it, re-rendered after every message.
template <class S, class V, class M> struct Holder : QWidget {
    S state;
    V* view;
    explicit Holder(S s) : state(std::move(s)), view(new V) {
        auto* l = new QVBoxLayout(this);
        l->setContentsMargins(0, 0, 0, 0);
        l->addWidget(view);
        view->render(state);
        QObject::connect(view, &V::message, view, [this](const M& m) {
            (void)state.update(m);
            view->render(state);
        });
        resize(1100, 900);
    }
};

std::string expect(QWidget& root, std::initializer_list<const char*> texts) {
    for (const auto* t : texts) {
        const auto want = QString::fromUtf8(t);
        bool found = false;
        for (const auto* l : root.findChildren<QLabel*>()) found = found || (l->text() == want && l->isVisible());
        for (const auto* b : root.findChildren<QAbstractButton*>())
            found = found || ((b->text() == want || b->accessibleName() == want) && b->isVisible());
        if (!found) return std::string("missing ") + t;
    }
    return {};
}

} // namespace

QWidget* settings_peers_screen() {
    namespace vp = loa::view::settings::peers;
    vp::Data d;
    d.node = vp::NodeCard{"own-key-0123456789", "This laptop", "OS keychain", "UDP 0.0.0.0:31415 \u00b7 TCP 0.0.0.0:31415", true, "on", true, "off", false, false};
    d.peers = {vp::Peer{"peer-key-0123456789", "Alice", vp::Discovery::Lan, vp::Presence::Online, std::nullopt, vp::Transport::Direct, "0.1.0", true}};
    d.nearby = {vp::Peer{"near-key-0123456789", "Bob", vp::Discovery::Tailscale, vp::Presence::Offline, 10, vp::Transport::None, "0.1.0", std::nullopt}};
    d.resources = {{"prompt", "Prompt A", vp::GrantScope::Prompt}, {"ssh", "Build host", vp::GrantScope::SshHost}};
    d.grants = {vp::Grant{"g1", "peer-key-0123456789", vp::GrantDirection::Given, {"prompt", "Prompt A", vp::GrantScope::Prompt, true}, vp::Permission::View, 7200, 3600, false, false},
                vp::Grant{"g2", "peer-key-0123456789", vp::GrantDirection::Received, {"s", "Plan", vp::GrantScope::Session, false}, vp::Permission::Fork, std::nullopt, std::nullopt, false, false}};
    d.remote_sessions = {vp::RemoteItem{"peer-key-0123456789", "Alice", "s1", "Plan", "omp", "view", false, true, true}};
    d.local_version = "0.1.0";
    peers::State s;
    s.set_data(std::move(d));
    (void)s.update(peers::Message::local(peers::local::SelectPeer{"peer-key-0123456789"}));
    (void)s.update(peers::Message::local(peers::local::SelectResource{"ssh"}));
    (void)s.update(peers::Message::local(peers::local::Ask{peers::confirm::Ssh{}}));
    auto* screen = new Holder<peers::State, peers::PeersView, peers::Message>(std::move(s));
    screen->setAccessibleName(QStringLiteral("peers"));
    return screen;
}

std::string check_settings_peers_screen(QWidget& screen) {
    return expect(screen, {"This node", "Alice  peer-key-012", "Discovered nearby", "Add peer", "Plan", "Grants with Alice", "Create grant",
                           "SSH access is off until explicitly confirmed.", "Confirm SSH access", "From Alice: Plan, fork \u00b7 no end date"});
}

QWidget* settings_updates_screen() {
    namespace vu = loa::view::settings::updates;
    vu::GateReport report{"19.0.0", "19.1.0", {{vu::GateCheck::ToolSchemas, vu::Verdict::Harmful, {"- edit: tool removed"}}, {vu::GateCheck::RpcProtocol, vu::Verdict::Pass, {}}}};
    vu::Product omp;
    omp.id = omp.label = "omp";
    omp.kind = vu::product_kind::Omp{};
    omp.source = "GitHub can1357/oh-my-pi";
    omp.current = "19.0.0";
    omp.installed = vu::installed::Managed{"/home/user/.local/bin/omp"};
    omp.last_check_unix = 100;
    omp.report = report;
    omp.held = vu::HeldUpdate{"19.1.0", report};
    omp.changelog = "- faster startup";
    vu::Product app;
    app.id = app.label = "app";
    app.kind = vu::product_kind::App{};
    app.current = "0.1.0";
    app.installed = vu::installed::Running{};
    app.last_check_unix = 100;
    app.pending_swap = app.offer = "0.2.0";
    app.staged = {"0.2.0"};
    vu::Data d{{omp, app}, 42, {vu::PeerStanding{"laptop", "laptop", "0.1.0", vu::standing::Behind{0, 1, 0}, {{"control", 1}}, true}}, false, 400};
    updates::State s(std::move(d));
    (void)s.update(updates::Message::local(updates::local::Toggle{"omp"}));
    auto* screen = new Holder<updates::State, updates::UpdatesView, updates::Message>(std::move(s));
    screen->setAccessibleName(QStringLiteral("updates"));
    return screen;
}

std::string check_settings_updates_screen(QWidget& screen) {
    return expect(screen, {"Check all", "19.1.0 held: 1 harmful check. Update anyway, pin or wait.", "Update anyway to 19.1.0", "Pin 19.0.0",
                           "Swap to 0.2.0", "Gate report 19.0.0 \u2192 19.1.0", "- edit: tool removed", "Release notes", "1 peer is behind.",
                           "behind by 1 minor"});
}

} // namespace loa::ui::app
