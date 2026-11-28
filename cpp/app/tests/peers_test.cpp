// The peers area (port of `loa-app/src/areas/peers/{tests,adapter}.rs`): grant requests and the
// grants they come back as, the node's network settings, and two apps whose nodes meet over
// loopback for the grant, remote session and shared folder flows.

// `loa/p2p/node.hpp` (through `peers.hpp`) declares a member named `emit`: before any Qt header.
#include "areas/peers/peers.hpp"

#include <chrono>
#include <filesystem>
#include <fstream>
#include <sstream>
#include <thread>

#include <doctest/doctest.h>

#include "loa/runtime/fs.hpp"
#include "loa/runtime/tempfile.hpp"
#include "support/test_app.hpp"

using namespace loa::app;
namespace pa = loa::app::areas::peers;
namespace panel = loa::view::settings::peers;
namespace trust = loa::trust;
namespace res = loa::trust::resource;
namespace p2p = loa::p2p;
namespace fs = std::filesystem;
using loa::app::testing::FakeHarness;
using loa::app::testing::TestApp;

namespace {

panel::GrantInput input(std::string peer, std::string id, panel::GrantScope scope, panel::Permission permission,
                        bool future_sessions = false, std::string label = "") {
    return panel::GrantInput{std::move(peer), panel::GrantResource{std::move(id), std::move(label), scope, future_sessions},
                             permission, 3600, false};
}

std::string read_file(const fs::path& path) {
    std::ifstream in(path, std::ios::binary);
    std::stringstream s;
    s << in.rdbuf();
    return s.str();
}

// One app whose node listens on loopback on free ports, discovery off.
struct Peer {
    TestApp test;

    explicit Peer(std::string_view name)
        : test(std::make_shared<loa::runtime::Executor>(2),
               FakeHarness("fake").with_capabilities(loa::contracts::Capabilities::builder().host_tools().mcp_servers().build()),
               name, [](loa::settings::app::Config&) {}) {
        auto network = test.app->ctx.services.storage.config.load().value().network;
        network.port = 0;
        auto config = pa::node_config(test.app->ctx, network);
        config.bind = "127.0.0.1:0";
        config.presence_interval = std::chrono::hours(1);
        test.run(area().reopen(test.app->ctx, std::move(config)));
        REQUIRE_MESSAGE(area().node() != nullptr, test.notice());
    }

    pa::State& area() { return test.area<pa::State>(); }
    p2p::Node& node() { return *area().node(); }
    std::string key() { return node().key().to_string(); }
    std::string udp() {
        auto status = node().status();
        const auto* l = std::get_if<p2p::endpoint::Listening>(&status.endpoint);
        REQUIRE_MESSAGE(l != nullptr, "the endpoint listens");
        return l->udp.at(0);
    }
    // Runs a panel intent as the Peers tab sends it.
    void intent(panel::Intent intent) { test.run(area().apply(std::move(intent), test.app->ctx)); }
    // Probes the peers until `done` holds.
    void until(std::string_view what, const std::function<bool(Peer&)>& done) {
        const auto deadline = std::chrono::steady_clock::now() + loa::app::testing::PATIENCE;
        while (!done(*this)) {
            REQUIRE_MESSAGE(std::chrono::steady_clock::now() < deadline, "timed out waiting for ", what);
            intent(panel::intent::Refresh{});
            test.pump_feeds();
            std::this_thread::sleep_for(std::chrono::milliseconds(50));
        }
    }
};

} // namespace

TEST_CASE("peers: resource ids round trip") {
    const std::vector<trust::Resource> resources{
        res::Session{"s-1"},
        res::Prompt{"s:with:colons", loa::core::NodeId{7}},
        res::Project{"/home/me/work"},
        res::Ssh{"box"},
        res::Surface{"surface://browser/tab"},
        res::Folder{"/srv/my share"},
        res::Sandbox{},
    };
    for (const auto& resource : resources) CHECK(pa::parse_resource(pa::resource_id(resource), 5) == resource);
    CHECK(pa::parse_resource("future", 42) == trust::Resource{res::FutureSessions{42}});
    CHECK_FALSE(pa::parse_resource("nonsense", 0).has_value());
    CHECK_FALSE(pa::parse_resource("prompt:x:s", 0).has_value());
}

TEST_CASE("peers: panel requests become scopes") {
    using P = panel::Permission;
    using S = panel::GrantScope;
    auto prompt = pa::grant_request(input("k", "prompt:3:s", S::Prompt, P::View, true), 1000);
    REQUIRE(prompt.has_value());
    CHECK(prompt->second == 1000 + 3'600'000);
    REQUIRE(prompt->first.size() == 2);
    CHECK(prompt->first[1].resource == trust::Resource{res::FutureSessions{1000}});
    auto ssh = pa::grant_request(input("k", "ssh:box", S::SshHost, P::Drive), 0);
    REQUIRE(ssh.has_value());
    CHECK(ssh->first[0].actions.contains(trust::Action::exec));
    CHECK_FALSE(pa::grant_request(input("k", "ssh:box", S::SshHost, P::Fork), 0).has_value());
    CHECK_FALSE(pa::grant_request(input("k", "sandbox", S::Sandbox, P::View), 0).has_value());
    auto session = pa::grant_request(input("k", "session:s", S::Session, P::Drive), 0);
    REQUIRE(session.has_value());
    CHECK(pa::permission(session->first[0].actions) == P::Drive);
    CHECK_FALSE(pa::grant_request(input("k", "unknown:x", S::Session, P::View), 0).has_value());
}

TEST_CASE("peers: grant inputs come back as the grants they issue") {
    using P = panel::Permission;
    using S = panel::GrantScope;
    std::array<std::uint8_t, 32> seed{};
    seed.fill(7);
    const auto peer = trust::NodeIdentity::from_secret_bytes(seed).public_key();
    struct Case {
        const char* id;
        S scope;
        P permission;
        bool future_sessions;
    };
    for (const auto& c : {Case{"prompt:3:s", S::Prompt, P::View, true}, Case{"session:s", S::Session, P::Drive, false},
                          Case{"ssh:box", S::SshHost, P::Drive, false}, Case{"sandbox", S::Sandbox, P::Drive, false},
                          Case{"project:p", S::Project, P::Fork, false}}) {
        CAPTURE(c.id);
        auto in = input(peer.to_string(), c.id, c.scope, c.permission, c.future_sessions, std::string("Label of ") + c.id);
        in.reshare = true;
        auto request = pa::grant_request(in, 1000);
        REQUIRE(request.has_value());
        p2p::GrantInfo info{trust::GrantId{"g"}, peer, p2p::GrantDirection::given, request->first, request->second,
                            std::nullopt, std::nullopt, in.reshare, false};
        const std::vector<std::pair<trust::Resource, std::string>> labels{{*pa::parse_resource(c.id, 1000), in.resource.label}};
        auto shown = pa::grant(info, labels);
        REQUIRE(shown.has_value());
        CHECK(shown->peer_key == in.peer_key);
        CHECK(shown->resource == in.resource);
        CHECK(shown->permission == in.permission);
        CHECK(shown->reshare == in.reshare);
        CHECK(shown->direction == panel::GrantDirection::Given);
        CHECK(shown->expires_at == 1 + in.ttl_seconds);
    }
}

TEST_CASE("peers: folder requests name the real folder; view reads, drive writes") {
    using P = panel::Permission;
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir.has_value());
    const auto real = loa::runtime::fs::canonicalize(dir->path()).value();
    fs::create_directory(real / "share");
    std::ofstream(real / "file.txt") << "x";
    const auto share = (real / "share").string();
    auto folder = [](const std::string& path, P permission) {
        return input("k", "folder:" + path, panel::GrantScope::Folder, permission);
    };
    // Typed with a trailing slash and a `..` detour: the grant names the real folder.
    auto typed = pa::grant_request(folder(share + "/../share/", P::View), 0);
    REQUIRE_MESSAGE(typed.has_value(), typed.error().to_string());
    CHECK(typed->first[0].resource == trust::Resource{res::Folder{share}});
    CHECK(typed->first[0].actions == trust::ActionSet{trust::Action::read});
    auto drive = pa::grant_request(folder(share, P::Drive), 0);
    REQUIRE(drive.has_value());
    CHECK(drive->first[0].actions == trust::ActionSet{trust::Action::read, trust::Action::write});
    CHECK(pa::permission(drive->first[0].actions) == P::Drive);
    for (const auto& refused : {std::string("share"), std::string("/no/such/folder"), (real / "file.txt").string()}) {
        CAPTURE(refused);
        CHECK_FALSE(pa::grant_request(folder(refused, P::View), 0).has_value());
    }
    CHECK_FALSE(pa::grant_request(folder(share, P::Fork), 0).has_value());
}

TEST_CASE("peers: recents ids name a peer's session") {
    CHECK(pa::recent_id("abc", "s-1") == "peer:abc:s-1");
    auto parsed = pa::parse_recent("peer:abc:s:1");
    REQUIRE(parsed.has_value());
    CHECK(parsed->first == "abc");
    CHECK(parsed->second == "s:1");
    CHECK_FALSE(pa::parse_recent("abc:s").has_value());
    CHECK(loa::app::peers::is_remote("peer:abc:s"));
    CHECK(pa::join("", "a") == "a");
    CHECK(pa::join("a/b", "c") == "a/b/c");
    CHECK(pa::size_text(12) == "12 B");
    CHECK(pa::size_text(1536) == "1.5 KiB");
    CHECK(pa::size_text(3 * 1024 * 1024) == "3.0 MiB");
}

TEST_CASE("peers: the node takes port 31415 and the relay policy from [network]") {
    TestApp test;
    loa::settings::app::NetworkSettings network;
    auto config = pa::node_config(test.app->ctx, network);
    CHECK(network.port == p2p::DEFAULT_PORT);
    CHECK(p2p::DEFAULT_PORT == 31415);
    CHECK(config.bind == "0.0.0.0:31415");
    CHECK(config.relay == p2p::RelayConfig::off());
    CHECK(config.mdns);
    CHECK(config.app_version == pa::APP_VERSION);
    CHECK(config.name == test.app->ctx.services.node.label);
    CHECK(config.sessions != nullptr);

    network.port = 4433;
    network.relay = loa::settings::app::Relay::from_string("https://relay.example.org");
    network.discovery = {false, false};
    config = pa::node_config(test.app->ctx, network);
    CHECK(config.bind == "0.0.0.0:4433");
    CHECK(config.relay == p2p::RelayConfig::self_hosted_at("https://relay.example.org"));
    CHECK_FALSE(config.mdns);
    CHECK_FALSE(config.tailscale.has_value());
    // No public relays: both `default` and `off` turn relaying off.
    CHECK(pa::relay_config(loa::settings::app::Relay::from_string("default")) == p2p::RelayConfig::off());
    CHECK(pa::relay_config(loa::settings::app::Relay::from_string("off")) == p2p::RelayConfig::off());
}

TEST_CASE("peers: the area opens its node, lists its tab and copies the key from the palette") {
    TestApp test;
    auto& area = test.area<pa::State>();
    REQUIRE(area.node() != nullptr);
    CHECK(area.panel_tabs() == std::vector<PanelTab>{PanelTab{"peers", "Peers", std::nullopt, false}});
    CHECK(area.panel().data.node.key == test.app->ctx.services.node.key);
    test.command("peers.copy_key");
    CHECK(test.notice() == "Key copied");
    test.send(pa::State::message(to::peers::Open{}));
    CHECK(test.app->ctx.shell.active_tab() == std::optional<std::string>("peers"));
}

TEST_CASE("peers: two nodes grant a session view and a shared folder, browse and save it") {
    Peer owner("owner");
    Peer guest("guest-node");
    REQUIRE(owner.key() != guest.key());

    owner.intent(panel::intent::AddPeer{guest.key(), guest.udp()});
    guest.intent(panel::intent::AddPeer{owner.key(), owner.udp()});
    owner.until("the guest online", [](Peer& p) { return !p.area().panel().data.peers.empty() &&
                                                         p.area().panel().data.peers[0].presence == panel::Presence::Online; });

    // A key check: a valid key that isn't this node's.
    owner.intent(panel::intent::ValidateKey{guest.key()});
    owner.intent(panel::intent::ValidateKey{owner.key()});
    owner.intent(panel::intent::ValidateKey{"nope"});

    // A session view.
    const auto session = owner.test.session("hello from the owner");
    owner.intent(panel::intent::CreateGrant{input(guest.key(), "session:" + session, panel::GrantScope::Session, panel::Permission::View)});
    CHECK(owner.test.notice() == "Grant created");
    const auto& given = owner.area().panel().data.grants;
    REQUIRE(given.size() == 1);
    CHECK(given[0].direction == panel::GrantDirection::Given);
    CHECK(given[0].resource.id == "session:" + session);
    CHECK(given[0].permission == panel::Permission::View);

    guest.until("the owner's session listed", [](Peer& p) { return !p.area().panel().data.remote_sessions.empty(); });
    CHECK(std::ranges::any_of(guest.area().panel().data.grants,
                              [](const panel::Grant& g) { return g.direction == panel::GrantDirection::Received; }));
    const auto recent = pa::recent_id(owner.key(), session);
    CHECK(std::ranges::find(guest.area().listed(), recent) != guest.area().listed().end());

    guest.test.send(pa::State::message(to::peers::OpenRecent{recent}));
    REQUIRE_MESSAGE(guest.area().remote().has_value(), guest.test.notice());
    const auto& remote = *guest.area().remote();
    CHECK(remote.is(owner.key(), session));
    CHECK_FALSE(remote.can_drive());
    CHECK(remote.title() != "");
    CHECK(guest.test.app->ctx.screen() == std::optional<std::string_view>(pa::State::ID));
    guest.test.send(pa::State::message(pa::msg::Remote{pa::remote_msg::Close{}}));
    CHECK_FALSE(guest.area().remote().has_value());
    CHECK_FALSE(guest.test.app->ctx.screen().has_value());

    // A shared folder the guest may write.
    auto dir = loa::runtime::TempDir::create();
    REQUIRE(dir.has_value());
    const auto shared = loa::runtime::fs::canonicalize(dir->path()).value();
    fs::create_directory(shared / "sub");
    std::ofstream(shared / "a.txt") << "hello";
    owner.intent(panel::intent::CreateGrant{
        input(guest.key(), "folder:" + shared.string(), panel::GrantScope::Folder, panel::Permission::Drive)});
    REQUIRE(owner.test.notice() == "Grant created");
    guest.until("the folder shared", [](Peer& p) { return !p.area().panel().data.shared_folders.empty(); });
    CHECK(guest.area().panel().data.shared_folders[0].can_write);

    guest.intent(panel::intent::BrowseFolder{owner.key(), shared.string()});
    REQUIRE_MESSAGE(guest.area().folder().has_value(), guest.test.notice());
    const auto& browser = *guest.area().folder();
    CHECK(browser.can_write);
    CHECK(browser.path.empty());
    CHECK(std::ranges::any_of(browser.entries, [](const p2p::FolderEntry& e) { return e.name == "a.txt" && !e.dir && e.size == 5; }));
    CHECK(std::ranges::any_of(browser.entries, [](const p2p::FolderEntry& e) { return e.name == "sub" && e.dir; }));

    auto folder = [&](pa::FolderMessage m) { guest.test.send(pa::State::message(pa::msg::Folder{std::move(m)})); };
    folder(pa::folder_msg::Enter{"sub"});
    CHECK(guest.area().folder()->path == "sub");
    CHECK(guest.area().folder()->entries.empty());
    folder(pa::folder_msg::Crumb{0});
    CHECK(guest.area().folder()->path.empty());

    folder(pa::folder_msg::Open{"a.txt"});
    REQUIRE(guest.area().folder()->file.has_value());
    CHECK(guest.area().folder()->file->preview == pa::Preview{std::string("hello")});
    folder(pa::folder_msg::Edit{"hello world"});
    CHECK(guest.area().folder()->file->dirty);
    folder(pa::folder_msg::Save{});
    CHECK(guest.test.notice() == "Saved a.txt on owner");
    CHECK_FALSE(guest.area().folder()->file->dirty);
    CHECK(read_file(shared / "a.txt") == "hello world");

    folder(pa::folder_msg::Close{});
    CHECK_FALSE(guest.area().folder().has_value());

    // Revoking ends the given grant.
    const auto id = owner.area().panel().data.grants[0].id;
    owner.intent(panel::intent::RevokeGrant{id});
    CHECK(owner.test.notice() == "Grant revoked");
}
