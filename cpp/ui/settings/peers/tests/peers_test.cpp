// Port of the tests in `crates/loa-ui-settings/src/peers/state.rs` (render tests not ported).

#include <doctest/doctest.h>

#include <format>

#include "loa/ui/settings/peers/state.hpp"

namespace peers_tests {

using namespace loa::ui::settings::peers;
namespace in = vp::intent;
namespace l = loa::ui::settings::peers::local;
using vp::Discovery;
using vp::Presence;

Peer peer(std::string key, std::string name, Discovery discovery, Presence presence, std::optional<std::uint64_t> last_seen, std::string version) {
    return Peer{std::move(key), std::move(name), discovery, presence, last_seen, vp::Transport::None, std::move(version), true};
}

State state() {
    Grant grant{"grant", "peer", GrantDirection::Given, GrantResource{"resource", "Prompt A", GrantScope::Prompt, true}, Permission::View, 7200, 3600, false, false};
    std::vector<ResourceChoice> resources{{"resource", "Prompt A", GrantScope::Prompt}, {"ssh", "Host", GrantScope::SshHost}};
    vp::RemoteItem remote{"peer", "Alice", "s1", "Plan", "omp", "view", false, false, true};
    vp::SharedFolderItem folder{"peer", "Alice", "/srv/share", false, true};
    std::vector<Peer> nearby{peer("near-z", "Zara", Discovery::Lan, Presence::Offline, 1, "1.0.0"),
                             peer("near-a", "Alice nearby", Discovery::Tailscale, Presence::Online, 2, "1.0.0"),
                             peer("peer", "Alice", Discovery::Lan, Presence::Online, std::nullopt, "1.0.0")};
    Data d;
    d.node.key = "own";
    d.peers = {peer("peer", "Alice", Discovery::Lan, Presence::Online, std::nullopt, "1.0.0")};
    d.nearby = std::move(nearby);
    d.grants = {grant};
    d.resources = std::move(resources);
    d.remote_sessions = {remote};
    d.shared_folders = {folder};
    State s;
    s.set_data(std::move(d));
    return s;
}

Intent input(std::string resource, std::string label, GrantScope scope, Permission permission, bool future_sessions) {
    return in::CreateGrant{GrantInput{"peer", GrantResource{std::move(resource), std::move(label), scope, future_sessions}, permission, 24 * 3600, false}};
}

Message L(Local m) { return Message::local(std::move(m)); }
Message E(Intent e) { return Message::event(std::move(e)); }

std::vector<Message> cat(std::vector<Message> a, const std::vector<Message>& b) {
    a.insert(a.end(), b.begin(), b.end());
    return a;
}

TEST_CASE("messages reduce to guarded requests") {
    using P = Permission;
    auto select = [](std::string id) { return std::vector<Message>{L(l::SelectPeer{"peer"}), L(l::SelectResource{std::move(id)})}; };
    auto remote = [](std::string session) -> Intent { return in::OpenRemote{"peer", std::move(session)}; };
    const Intent fork = in::ForkRemote{"peer", "s1"};
    auto browse = [](std::string folder) -> Intent { return in::BrowseFolder{"peer", std::move(folder)}; };
    auto ask = [](Confirm c) { return L(l::Ask{std::move(c)}); };
    const std::vector<std::pair<std::vector<Message>, std::vector<Intent>>> cases{
        {{L(l::Key{"key-one"}), L(l::Key{"key-two"}), L(l::AddPeer{})}, {in::ValidateKey{"key-one"}, in::ValidateKey{"key-two"}}},
        {cat(select("ssh"), {L(l::SubmitGrant{}), L(l::Confirm{}), ask(confirm::Ssh{}), L(l::Confirm{}), L(l::SubmitGrant{}), L(l::Ttl{"0"}), L(l::SubmitGrant{})}),
         {input("ssh", "Host", GrantScope::SshHost, P::View, false)}},
        {cat(select("ssh"), {ask(confirm::Ssh{}), L(l::SelectResource{"resource"}), L(l::SelectResource{"ssh"}), L(l::Confirm{}), L(l::SubmitGrant{})}), {}},
        {cat(select("resource"), {L(l::FutureSessions{true}), L(l::SetPermission{P::View}), L(l::SubmitGrant{})}),
         {input("resource", "Prompt A", GrantScope::Prompt, P::View, true)}},
        {cat(select("resource"), {L(l::SubmitGrant{})}), {input("resource", "Prompt A", GrantScope::Prompt, P::Fork, false)}},
        {{L(l::SelectPeer{"peer"}), L(l::UseFolder{}), L(l::FolderPath{" /srv/share "}), L(l::UseFolder{}), L(l::SetPermission{P::Fork}), L(l::SubmitGrant{}),
          L(l::SetPermission{P::Drive}), L(l::SubmitGrant{})},
         {input("folder:/srv/share", "Folder /srv/share", GrantScope::Folder, P::Drive, false)}},
        {{ask(confirm::RemovePeer{"peer"}), L(l::Cancel{}), L(l::Confirm{}), ask(confirm::RemovePeer{"peer"}), L(l::Confirm{})}, {in::RemovePeer{"peer"}}},
        {{L(l::BeginRename{"peer"}), L(l::RenameText{"  "}), L(l::SaveRename{}), L(l::BeginRename{"peer"}), L(l::RenameText{" New name "}), L(l::SaveRename{})},
         {in::RenamePeer{"peer", "New name"}}},
        {{ask(confirm::RevokeGrant{"other"}), L(l::Confirm{}), ask(confirm::RevokeGrant{"grant"}), L(l::Confirm{})}, {in::RevokeGrant{"grant"}}},
        {{E(in::RenewGrant{"grant"}), E(in::RenewGrant{"other"}), E(in::CopyKey{"own"}), E(in::CopyKey{"stranger"}), E(in::ScanTailscale{}), E(in::Refresh{})},
         {in::RenewGrant{"grant"}, in::CopyKey{"own"}, in::Refresh{}}},
        {{E(remote("s1")), E(remote("s2")), E(fork), E(browse("/srv/share")), E(browse("/etc")), E(in::AddDiscovered{"peer"}), E(in::AddDiscovered{"near-a"})},
         {remote("s1"), browse("/srv/share"), in::AddDiscovered{"near-a"}}},
    };
    for (std::size_t i = 0; i < cases.size(); ++i) {
        CAPTURE(i);
        auto s = state();
        std::vector<Intent> events;
        for (const auto& m : cases[i].first)
            if (auto e = s.update(m)) events.push_back(std::move(*e));
        CHECK(events == cases[i].second);
    }
}

TEST_CASE("snapshots keep only backed input") {
    auto s = state();
    s.set_key_validation("key", true);
    (void)s.update(L(l::Key{"key"}));
    s.set_key_validation("key", true);
    (void)s.update(L(l::Address{" 127.0.0.1:31415 "}));
    CHECK(s.update(L(l::AddPeer{})) == std::optional<Intent>(in::AddPeer{"key", "127.0.0.1:31415"}));
    for (Local m : {Local{l::SelectPeer{"peer"}}, Local{l::SelectResource{"resource"}}, Local{l::Ask{confirm::RevokeGrant{"grant"}}}}) (void)s.update(L(m));
    auto snapshot = s.data;
    snapshot.grants.clear();
    snapshot.resources.clear();
    s.set_data(std::move(snapshot));
    CHECK(!s.update(L(l::Confirm{})));
    CHECK(!s.update(L(l::SubmitGrant{})));
    s.data.remote_sessions[0].can_fork = true;
    s.data.node.tailscale_available = true;
    s.data.shared_folders[0].online = false;
    std::vector<Intent> events;
    for (Intent e : {Intent{in::ForkRemote{"peer", "s1"}}, Intent{in::ScanTailscale{}}, Intent{in::BrowseFolder{"peer", "/srv/share"}}})
        if (auto r = s.update(E(e))) events.push_back(*r);
    CHECK(events == std::vector<Intent>{in::ForkRemote{"peer", "s1"}, in::ScanTailscale{}});
}

TEST_CASE("lists filter, sort and describe") {
    auto s = state();
    auto keys = [](const std::vector<const Peer*>& list) {
        std::vector<std::string> out;
        for (const auto* p : list) out.push_back(p->key);
        return out;
    };
    s.sort = PeerSort::OnlineFirst;
    CHECK(keys(s.visible_nearby()) == std::vector<std::string>{"near-a", "near-z"});
    s.filter = "zara";
    CHECK(keys(s.visible_nearby()) == std::vector<std::string>{"near-z"});
    const std::vector<Peer> peers{peer("z", "Zed", Discovery::Lan, Presence::Offline, 20, "1.0.0"), peer("a", "Alice", Discovery::Lan, Presence::Online, 10, "2.0.0"),
                                  peer("b", "Bob", Discovery::Lan, Presence::Offline, 30, "1.5.0")};
    CHECK(keys(visible(peers, "ALi", PeerSort::Name)) == std::vector<std::string>{"a"});
    CHECK(keys(visible(peers, "", PeerSort::OnlineFirst)) == std::vector<std::string>{"a", "b", "z"});
    CHECK(keys(visible(peers, "", PeerSort::RecentlySeen)) == std::vector<std::string>{"b", "z", "a"});
    CHECK(keys(visible(peers, "", PeerSort::Version)) == std::vector<std::string>{"a", "b", "z"});
    const auto& grant = state().data.grants[0];
    CHECK(grant_summary(grant) == "Prompt A and every session from now on, view");
    CHECK(grant.expiry_text(0) == "expires in 2h; renews in 1h");
    GrantForm form;
    for (auto [scope, permission] : {std::pair{GrantScope::Surface, Permission::View}, std::pair{GrantScope::SshHost, Permission::View},
                                     std::pair{GrantScope::Sandbox, Permission::Drive}, std::pair{GrantScope::Session, Permission::Fork}}) {
        form.select_resource(ResourceChoice{std::format("{}", static_cast<int>(scope)), "", scope});
        CHECK(form.permission == permission);
    }
}

TEST_CASE("grant form errors") {
    GrantForm form;
    CHECK(form.build("peer").error() == GrantFormError::MissingResource);
    form.select_resource(ResourceChoice{"s", "Host", GrantScope::SshHost});
    form.permission = Permission::Fork;
    CHECK(form.build("peer").error() == GrantFormError::InvalidPermission);
    form.permission = Permission::View;
    CHECK(form.build("peer").error() == GrantFormError::SshNotConfirmed);
    form.ssh_confirmed = true;
    for (const char* ttl : {"0", "abc", "", "-1", "18446744073709551615"}) {
        form.ttl_hours = ttl;
        CHECK(form.build("peer").error() == GrantFormError::InvalidTtl);
    }
    form.ttl_hours = " 2 ";
    REQUIRE(form.build("peer"));
    CHECK(form.build("peer")->ttl_seconds == 7200);
    CHECK(to_string(GrantFormError::SshNotConfirmed) == "SshNotConfirmed");
    CHECK(grant_summary(Grant{"g", "p", GrantDirection::Given, GrantResource{"f", "x", GrantScope::FutureSessions, false}, Permission::Fork, {}, {}, false, false}) ==
          "Every future session, fork");
}

TEST_CASE("confirm transitions") {
    auto s = state();
    // SSH confirmation needs an SSH resource picked.
    (void)s.update(L(l::Ask{confirm::Ssh{}}));
    CHECK(!s.confirm);
    (void)s.update(L(l::SelectPeer{"peer"}));
    (void)s.update(L(l::SelectResource{"ssh"}));
    (void)s.update(L(l::Ask{confirm::Ssh{}}));
    CHECK(s.confirm == std::optional<Confirm>(confirm::Ssh{}));
    // Selecting the peer again drops it.
    (void)s.update(L(l::SelectPeer{"peer"}));
    CHECK(!s.confirm);
    (void)s.update(L(l::SelectResource{"ssh"}));
    (void)s.update(L(l::Ask{confirm::Ssh{}}));
    CHECK(!s.update(L(l::Confirm{})));
    CHECK(s.grant_form.ssh_confirmed);
    CHECK(!s.confirm);
    (void)s.update(L(l::Ask{confirm::RemovePeer{"nobody"}}));
    CHECK(!s.confirm);
}

} // namespace peers_tests
