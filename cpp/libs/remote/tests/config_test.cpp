// Config entries resolve hop by hop; limits refuse before anything is sent; edits replace one
// occurrence.

#include <doctest/doctest.h>

#include <algorithm>

#include "loa/remote/config.hpp"
#include "loa/remote/limits.hpp"
#include "loa/remote/resolve.hpp"
#include "loa/remote/service.hpp"

using namespace loa::remote;

TEST_CASE("entries resolve hop by hop and import keeps limits") {
    Static ssh("me");
    ssh.with("bastion", "10.0.0.1", 22).with("db", "db.internal", 22, "bastion,jump@[::1]:2200");
    ssh.with("a", "a", 22, "b").with("b", "b", 22, "a");
    std::map<std::string, RemoteHost> entries;
    CHECK(import_aliases(entries, ssh) == std::vector<std::string>{"a", "b", "bastion", "db"});
    CHECK(entries["db"].read_only);
    CHECK_FALSE(entries["db"].commands);
    CHECK(entries["db"].allowed_paths.empty());
    entries["db"].commands = true;
    auto manual = entry_for_alias("");
    manual.host = "192.168.1.5";
    manual.user = "root";
    manual.port = 2222;
    manual.proxy_jump = "bastion";
    manual.auto_approve = true;
    entries.emplace("box", manual);
    CHECK_MESSAGE(import_aliases(entries, ssh).empty(), "re-import adds nothing");
    CHECK_MESSAGE(entries["db"].commands, "and keeps limits");

    const auto [hosts, failed] = load(entries, ssh);
    REQUIRE(failed.size() == 2);
    CHECK(failed[0].first == "a");
    CHECK(failed[0].second.kind() == ErrorKind::proxy_jump_cycle);
    CHECK(failed[1].first == "b");
    CHECK(failed[1].second.kind() == ErrorKind::proxy_jump_cycle);
    const auto db = std::ranges::find(hosts, "db", &Host::name);
    REQUIRE(db != hosts.end());
    std::vector<std::tuple<std::string, std::uint16_t, std::string>> hops;
    for (const auto& e : db->hops()) hops.emplace_back(e.hostname, e.port, e.user);
    CHECK(hops == std::vector<std::tuple<std::string, std::uint16_t, std::string>>{
                      {"10.0.0.1", 22, "me"}, {"::1", 2200, "jump"}, {"db.internal", 22, "me"}});
    const auto b = std::ranges::find(hosts, "box", &Host::name);
    REQUIRE(b != hosts.end());
    CHECK(b->target.hostname == "192.168.1.5");
    CHECK(b->target.port == 2222);
    CHECK(b->target.user == "root");
    CHECK(b->jumps.size() == 1);
    CHECK(b->limits.auto_approve_writes);
    CHECK(b->limits.auto_approve_commands);
    for (const auto* bad : {"", "h:0", "[::1", "[::1]x"}) {
        CAPTURE(bad);
        CHECK_FALSE(parse_jump(bad));
    }
    CHECK(parse_jump("ssh://u@h:22") == std::tuple{std::string("h"), std::optional<std::string>("u"), std::optional<std::uint16_t>(22)});
    CHECK(parse_jump("[::1]:2200").value() == std::tuple{std::string("::1"), std::optional<std::string>(), std::optional<std::uint16_t>(2200)});
    CHECK(parse_jump("").error() == "ProxyJump ``: empty host");
}

TEST_CASE("identity agent keeps the named pipe path and maps none and the default") {
    ResolvedHost r;
    r.identity_agent = R"(\\.\pipe\openssh-ssh-agent)";
    CHECK(endpoint("a", r).agent == AgentSocket::at(R"(\\.\pipe\openssh-ssh-agent)"));
    r.identity_agent = "NONE";
    CHECK(endpoint("a", r).agent == AgentSocket::none());
    r.identity_agent = "$SSH_AUTH_SOCK";
    CHECK(endpoint("a", r).agent == AgentSocket{});
}

TEST_CASE("paths and actions are checked before anything is sent") {
    HostLimits srv;
    srv.allowed_paths = {"/srv/app/", "rel"};
    HostLimits all;
    all.allowed_paths = {"/"};
    const std::vector<std::tuple<const HostLimits*, const char*, bool>> cases{
        {&srv, "/srv/app/../../etc/shadow", false}, {&srv, "/srv/app/./a//b", true}, {&srv, "/srv/application", false},
        {&srv, "/rel/x", false}, {nullptr, "/srv/x", false}, {&all, "/etc/hosts", true},
    };
    const HostLimits none;
    for (const auto& [limits, path, ok] : cases) {
        CAPTURE(path);
        CHECK((limits ? *limits : none).check_path(*normalize(path)).has_value() == ok);
    }
    CHECK(all.allows_every_path());
    CHECK_FALSE(srv.allows_every_path());
    CHECK(normalize("/../..") == "/");
    CHECK_FALSE(normalize("relative/x"));
    CHECK(normalize("relative/x").error() == "path `relative/x` is not absolute");
    CHECK_FALSE(normalize(std::string_view("/a\0b", 4)));
    CHECK(split("/a/b/c") == std::pair<std::string_view, std::string_view>{"/a/b", "c"});
    CHECK(split("/a") == std::pair<std::string_view, std::string_view>{"/", "a"});
    CHECK(none.check_path("/srv/x").error() == "/srv/x refused: no paths are allowed on this host");
    CHECK(srv.check_path("/etc").error() == "/etc is outside the allowed paths (/srv/app/, rel)");

    HostLimits ro;
    ro.read_only = true;
    HostLimits no_cmd;
    no_cmd.commands = false;
    const auto allowed = [](const HostLimits& l) {
        std::vector<Action> out;
        for (const auto a : ALL_ACTIONS) {
            if (l.check_action(a)) out.push_back(a);
        }
        return out;
    };
    CHECK(allowed(ro) == std::vector<Action>{Action::read, Action::list});
    CHECK(allowed(no_cmd) == std::vector<Action>{Action::read, Action::write, Action::list, Action::edit, Action::terminal});
    CHECK(ro.check_action(Action::write).error() == "host is read only; write refused");
    CHECK(no_cmd.check_action(Action::command).error() == "commands are not allowed on this host");
}

TEST_CASE("edits replace exactly one occurrence") {
    CHECK(apply_edit("abc", "b", "X") == "aXc");
    CHECK(apply_edit("abc", "a", "") == "bc");
    CHECK(apply_edit("abc", "", "X").error() == "edit needs a non-empty `old` string");
    CHECK(apply_edit("abc", "z", "X").error() == "`old` string not found");
    CHECK(apply_edit("a=b=c", "=", ":").error() == "`old` string occurs 2 times; it must be unique");
}

TEST_CASE("errors read like the Rust Display") {
    CHECK(Error::denied(std::nullopt).message() == "denied by the user");
    CHECK(Error::denied("not now").message() == "denied by the user: not now");
    CHECK(Error::auth_failed("me", "h:22", 2).message() == "authentication as me@h:22 failed: none of the 2 agent identities was accepted");
    CHECK(Error::proxy_jump_too_deep("x").message() == "ProxyJump chain for `x` is deeper than 16 hops");
    CHECK(Error::too_large("/f", 9, 4).message() == "file /f is 9 bytes, over the 4 byte limit");
    CHECK(Error::no_agent("h:22").message() == "no ssh-agent socket for h:22: set IdentityAgent or SSH_AUTH_SOCK");
    CHECK(system_known_hosts(std::nullopt) == "/etc/ssh/ssh_known_hosts");
    const auto config = RemoteServiceConfig::from_home("/home/u");
    CHECK(config.known_hosts == std::vector<std::filesystem::path>{"/home/u/.ssh/known_hosts", "/home/u/.ssh/known_hosts2", "/etc/ssh/ssh_known_hosts"});
    CHECK(config.known_hosts_write == "/home/u/.ssh/known_hosts");
}
